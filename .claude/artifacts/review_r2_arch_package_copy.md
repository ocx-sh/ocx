# Round 2 — Architecture / ADR review: `ocx package copy`

**Scope:** `dfcdcb98..HEAD` on `evelynn` — the review-fix loop on `ocx package copy`.
**Lens:** architecture and ADR compliance only. Correctness, security, spec and test
coverage are other reviewers' surfaces.
**Method:** file reads, not `git diff` (the proxy swallows it). Every `file:line` below
was opened with `Read` and re-read after the claim was written.

**Explicitly out of scope, not reviewed:** the four pre-existing Invariant #5 reads on
the push/announce paths (`publisher.rs:274,287`, `publisher/publish_gate.rs:141`,
`announce/pipeline.rs:329,399,431`). Known, deliberate, already escalated.

---

## 1. Inverting `ReadAddressing`'s default — **the right axis. Confirmed.**

### What the shape actually is

`crates/ocx_lib/src/oci/client.rs:139-157` defines the enum; `:350-357` is the one
routing switch. Five read methods now come in pairs — a short canonical form plus an
`_addressed` form (`list_tags` / `list_tags_addressed` at `:421-440`, `fetch_manifest` /
`fetch_manifest_addressed` at `:474-499`, `fetch_manifest_raw_bytes` /
`..._addressed` at `:2049-2069`, `pull_description` / `..._addressed` at `:1752-1771`).

### Judgement: correct, and the copy path is the proof

The decisive question is not "which default is more common" but **which direction a
forgotten decision fails in**. Under the new default a forgotten decision yields a
canonical read: correct, merely slower. Under the old one it yielded a mirrored read
that could decide a canonical write — CWE-345/367, silent, and invisible at the call
site. That asymmetry is stated at `client.rs:141-149` and it is the correct reading.

The strongest evidence is in this diff's own code. `crates/ocx_lib/src/publisher/copy.rs:439`
is a plain `client.list_tags(request.target.clone())`. That listing feeds
`resolve_cascade_tags` and decides which rolling tags get re-pointed at the target
(`:433-443`, doc at `:420-432`). Under the *old* default that exact line — the shortest,
most natural spelling — would have silently routed a promotion decision through a mirror.
The author wrote the obvious thing and got the safe thing. That is what a well-chosen
default buys.

### Has the mirror feature (Differentiator #9) been made harder to use?

**No, and the reason is structural rather than disciplinary.** The mirror-critical hot
path takes no `ReadAddressing` argument at all — it is unconditionally mirrored through
`Client::transport_reference`:

| Method | Line | Addressing |
|---|---|---|
| `head_blob` | `client.rs:726` | mirrored, unconditional |
| `pull_manifest` | `client.rs:752` | mirrored, unconditional |
| `pull_blob` | `client.rs:788` | mirrored, unconditional |
| `pull_layer` (blob stream) | `client.rs:894` | mirrored, unconditional |
| `fetch_manifest_digest` | `client.rs:454` | mirrored, unconditional |

No call site on the pull/install path can forget the mirror, because there is nothing to
forget — the parameter does not exist there. And it is safe for exactly these: every one
is digest-addressed and self-verifying, so a hostile mirror cannot substitute content.

The index sources — the other half of `[mirrors]` — name `Mirrored` explicitly at every
read (`oci/index/oci_index.rs:52,69,102`; `oci/index/ocx_index.rs:1088,1146`). Those are
five lines in two files that a reviewer of an index change will see. This is not
mirror-awareness scattered across call sites that will forget; it is concentrated where
the feature lives.

I tried to refute this by looking for a tag-addressed, write-deciding read that is
*unconditionally* mirrored with no canonical escape — that would be a hole the inversion
created and cannot close. `Client::fetch_manifest_digest` (`client.rs:453-466`) is the
only candidate: tag-capable, unconditionally mirrored, no `_addressed` variant. Its sole
production caller is `oci/index/oci_index.rs:83`, the derived-index resolution read,
which decides no write. The refutation fails. No finding.

### Is there a better third shape?

Yes in the abstract — a method that takes `ReadAddressing` with no default, so the host
is named at both ends. **The tree already has exactly one**, and its existence is the
one real finding here.

---

### [Warn] `probe_manifest_digest_addressed`'s doc comment states a fact that is false, and it is the fact the asymmetry rests on

`crates/ocx_lib/src/oci/client.rs:1990-1992`:

> Unlike its siblings this one has no short, canonical-by-default form:
> **every caller today wants a mirror**, and an unused wrapper is not kept.
> The host is therefore always named at the call site.

Three of its five production callers ask for `Canonical`:

- `crates/ocx_lib/src/package/cascade/apply.rs:205` — the child-manifest presence probe
  before a repair PUT.
- `crates/ocx_lib/src/package/cascade/apply.rs:350` — the read-modify-write race check
  immediately before `push_index` at `:378`.
- `crates/ocx_lib/src/package/cascade/apply.rs:404` — `verify_write`'s read-back.

The other two are `Mirrored` (`announce/pipeline.rs:399`,
`managed_config/persistence.rs:433`). So the split is 3 canonical / 2 mirrored, and all
three canonical ones are textbook Invariant #5 write-deciding reads — `apply.rs:350` is
one statement away from the PUT it guards.

**Principle at stake:** `quality-rust.md` "Comment Quality" — a `///` states the
contract; a stale one is Block-tier when it contradicts reality. Here the false clause
is load-bearing: it is the *entire stated justification* for this method diverging from
the four-sibling pattern.

**Concrete consequence:** the next author who needs a canonical probe reads this doc,
concludes canonical is not a supported mode for this method, and either adds a
`probe_manifest_digest` short form that is canonical-by-default (silently making the
inconsistency worse) or reaches for `fetch_manifest_addressed` and pulls a manifest body
where a HEAD would do.

**Refutation attempted:** I checked whether `apply.rs`'s canonical arguments could be
recent additions the doc simply predates, which would make it merely stale rather than
wrong. That does not rescue it — the module doc at `cascade/apply.rs:22` and the sibling
doc at `cascade.rs:307` both independently assert that this module addresses the
canonical registry, so the canonical callers are the deliberate, documented design and
the client-side doc is the outlier. It is wrong today regardless of when it became wrong.

**Fix (one line):** replace the clause with what is actually true — "callers are split
between the two, so there is no default worth defaulting to; the host is named at the
call site." That turns an incorrect rationale into the *correct* one, which is the
stronger argument anyway (see below).

---

### [Suggest] Two shapes now coexist for one decision

Four read methods use paired-wrapper (safe default + `_addressed` opt-in); one uses
no-default (`probe_manifest_digest_addressed`). Both shapes are defensible; having both,
undeclared, is the cost.

I am **not** recommending converging them, and the reason is worth writing down so a
later pass does not "fix" it:

- The paired-wrapper shape earns its keep on the four high-traffic methods precisely
  because 15 call sites is enough that forcing every one to name a host is churn that
  buys nothing — the default already fails in the safe direction.
- The no-default shape earns its keep on `probe_manifest_digest_addressed` because its
  callers genuinely split 3/2, so no default is more right than the other, and a wrapper
  would let a call site be silent about a choice that is genuinely open.

The gap is that this reasoning lives nowhere. Add one sentence to the `ReadAddressing`
doc at `client.rs:139-149` stating the selection rule — *a method whose callers are
predominantly one way gets a wrapper defaulting to canonical; a method whose callers
split gets no default* — and the inconsistency becomes a documented policy instead of an
accident. Cheaper than either conversion.

**No blocker on this judgement.** The inversion is the right call on the right axis, and
the `probe_manifest_digest` asymmetry is defensible — it is only the stated reason that
is wrong.

---

## 2. `push_blob_from_path` as a required trait method — **correct. No finding.**

### The decision

`crates/ocx_lib/src/oci/client/transport.rs:206-212`, with the rationale at `:193-203`:

> The obvious default — read the file, delegate to `Self::push_blob` — allocates the
> whole blob, which is the one thing this method exists to avoid, so a future transport
> that never noticed the method would compile and quietly reintroduce the allocation.

That is the right test for whether a default is earned, and the file applies it in both
directions on adjacent lines: `mount_blob` at `:224-232` **keeps** its default, because
`UploadRequired` is genuinely correct for a transport with no mounting. A rule that
produced the same answer for both would be a rule about ceremony; this one discriminates.

The ADR's own risk register (`adr_package_copy.md`, "Unbounded memory on large layers")
names the failure this guards: 100–200 MB layers × concurrent platforms, which is the
allocation PKG-04 exists to prevent. A silent default would make that regression
invisible — it compiles, the tests pass, and memory is not a test assertion.

### The cost, counted

Nine test doubles implement it. Five are `unimplemented!()`:

| Double | Line | Sibling `push_blob` on the lines above |
|---|---|---|
| `transport.rs` default-impl test | `:445` | `unimplemented!()` at `:442` |
| `referrer/capability.rs` | `:323` | `unimplemented!()` at `:320` |
| `verify/pipeline.rs` | `:2927` | `unimplemented!("verify never pushes")` at `:2924` |
| `verify/pipeline.rs` (SBOM) | `:4214` | `unimplemented!("reading an SBOM never pushes")` at `:4211` |
| `client.rs` | `:3934` | `unimplemented!()` at `:3931` |

Four delegate to the stated-buffering helper: `client/test_transport.rs:421`,
`attest/pipeline.rs:720`, `sign/pipeline.rs:567`, `client.rs:6604`.

### Judgement on the latent-panic question — **it is not one**

LINT-05 bans `todo!()`/`unimplemented!()` **reachable on a production path**. Two
independent reasons neither applies here:

1. Every one of the nine doubles is inside a `#[cfg(test)] mod tests` (verified for
   `referrer/capability.rs`: the only `#[cfg(test)]` in the file is at `:182`, and the
   double at `:323` is inside it). Production code cannot reach any of them.
2. **In all five cases the sibling `push_blob` directly above is already
   `unimplemented!()`.** The double already panics if that transport is ever asked to
   push anything. The new method adds a ninth line to an existing panic surface; it
   creates no new *class* of reachable panic. A double that would panic on
   `push_blob_from_path` would already have panicked on `push_blob` one call earlier.

The buffering escape hatch is `cfg(test)`-gated (`transport.rs:285-298`,
re-exported under `#[cfg(test)]` at `client.rs:119-120`), so a production transport
cannot reach the shape the default would have handed it. That closure is what makes
"required" cost only test lines.

**Refutation attempted:** the alternative the review brief names — "delete the default
and let it be non-defaulted only where it matters" — is not expressible. Rust has no
per-implementor default. The realizable alternatives are (a) a default that buffers,
which is the defect, or (b) `Option<...>`/a capability probe, which moves a compile-time
guarantee to runtime for no benefit. Required is the only shape that makes the
regression a build failure.

**Verdict: correct as built.** No change requested.

---

## 3. `CopyError`'s three-layer reshape — **diverges from the siblings, and the divergence is what shipped the regression**

### It does not match `SignError`/`VerifyError`. It matches them in outline and differs in the one arm that matters.

The doc at `crates/ocx_lib/src/publisher/copy.rs:30` claims parity: *"Three-layer,
matching `SignError`/`VerifyError`"*. The outer struct genuinely does match — `#[source]`
on `kind` (`:44-45`), `Display` naming only the context (`:37`), a `ClassifyErrorKind`
impl with an exhaustive `kind_detail()` (`:140-150`). All correct.

The **catch-all arm** does not match:

| | catch-all variant | attribute | `source()` from the kind reaches | `classify()` on the wrapper |
|---|---|---|---|---|
| `SignErrorKind` | `Internal` (`sign/error.rs:257`) | `#[error("internal signing error")]` + `#[source]` | the wrapped error | `None` (`sign/error.rs:53`) |
| `VerifyErrorKind` | `Internal` (`verify/error.rs:515`) | `#[error("internal verification error")]` + `#[source]` | the wrapped error | `None` (`verify/error.rs:46`) |
| `CopyErrorKind` | `Registry` (`publisher/copy.rs:118-119`) | `#[error(transparent)]` + `#[from]` | **the wrapped error's own source — skipping it** | explicit delegation (`:65`) |

`#[error(transparent)]` forwards `Display` *and* `source()` straight through, so
`CopyErrorKind::Registry.source()` returns what `crate::Error`'s source returns, never
`crate::Error` itself. And `crate::Error::OciClient` is *also* `#[error(transparent)]`
(`crates/ocx_lib/src/error.rs:95-96`), so a chain walk starting at `CopyError` skips
**two** levels at once and never visits the `ClientError` that knows its own exit code.
That is precisely the 79/80/84 → 1 collapse.

### So: is the *pattern* the trap?

**No. The pattern is fine; this instance departed from it.** `SignError`/`VerifyError`
carry no latent form of this defect, for a reason that is structural rather than lucky:
their catch-all uses a named message plus `#[source]`, which makes the wrapped error
reachable, which is what lets `classify()` safely return `None` and defer to
`try_classify`'s downcast ladder (`cli/classify.rs:104-155`).

I looked specifically for a counterexample and found the near-miss:
`VerifyErrorKind::TrustPolicyInvalid` at `verify/error.rs:271-272` **is**
`#[error(transparent)]` + `#[from]`. It is harmless, and why is the whole rule: it maps
to a **fixed** code (`ConfigError`, `verify/error.rs:665-666`), so `VerifyError::classify`
answers `Some(ConfigError)` and never needs to walk into it.

The trap is therefore narrower and nameable than "the three-layer pattern":

> **`#[error(transparent)]` on an arm whose exit code is *delegated to the cause*.**
> Transparent + fixed code is safe. Named + `#[source]` + delegated is safe. Only
> transparent + delegated is broken, and it is broken silently — the kind is still
> reachable by `matches!`, so a test asserting the variant passes while every exit code
> is wrong.

That last sentence is already written down at `publisher/copy.rs:1035-1039`, and the
regression test at `:1040-1066` discriminates correctly — it asserts
`Some(ExitCode::NotFound)`, not merely that the variant matched. Good test.

### [Suggest] The fix is right; the shape it preserves is still the weaker of the two

`CopyError::classify` (`:62-68`) compensates by delegating explicitly instead of
returning `None`, and the doc at `:58-61` states exactly why. That works and is well
argued. But it leaves `CopyErrorKind::Registry` as the only arm in the family where
`kind.source()` does not reach the value the kind wraps — a property no other error type
here has, guarded only by a doc comment and one test.

The sibling shape removes the hazard rather than documenting it:

```rust
#[error("registry operation failed")]
Registry(#[source] crate::Error),
```

…which would make `classify()`'s `None` arm safe and bring the arm into literal parity
with `Internal`. The cost is one extra line in the rendered `{err:#}` chain and losing
`#[from]` (the two `From` impls at `:153-163` already exist and would absorb it).

Not a blocker, and I would not insist: the explicit-delegation form is correct, tested,
and its reasoning is written at the call site. But if a future arm on `CopyErrorKind`
ever wants to defer to its cause, it will hit the same trap, and the doc at `:58-61`
guards only the arm it sits next to.

### [Suggest] The chain is four layers, not three

`CopyError` → `CopyErrorKind::Registry` → `crate::Error::OciClient` → `ClientError`.
The `crate::Error` hop earns its place — `CopyErrorKind::Registry` has to absorb index
errors, digest errors and client errors, and `crate::Error` is the existing union for
exactly that, so collapsing it would mean either three `From` impls into three new
variants or an erased `Box<dyn Error>`. Reusing the union is the cheaper, more
conventional choice.

The cost is that the hop is *invisible*: both it and the arm above it are transparent, so
the rendered chain shows three layers while the type graph has four. That is only a
problem for someone debugging the classification path — which is exactly who was
debugging it last week. One line in `CopyErrorKind::Registry`'s doc naming
`crate::Error` as the intermediate would have saved that.

---

## 4. ARCH-01 / ARCH-03 / ARCH-12 — no new violations

**ARCH-01 (a repeated leading parameter tuple is a missing type): actively applied, not
merely avoided.** `crates/ocx_lib/src/oci/copy.rs:260-269` introduces `Transfer<'a>`
binding `(client, source, target, scratch)`, and the doc names the rule by ID:

> Five functions threaded `(client, source, target, …, scratch)` in that order
> (ARCH-01). `source` and `target` are the same type, so transposing them is a one-token
> edit with no compiler objection behind it — a copy that reads from the target and
> writes to the source.

That second sentence is the better argument. A same-type parameter pair is not just a
naming smell; it is an unguarded transposition on a path where the transposition means
"promote production into staging". Correct call, correctly motivated.

**ARCH-03 (≤2 inherent `impl` blocks, ≤25 methods): the block half holds; the method half
was already blown before this diff.** `oci/client.rs` has exactly one `impl Client` block
(`:186`) — no new block was added. Its method count is well past 25 and has been for a
long time; this diff adds roughly six (`read_reference`, and the four `_addressed`
variants plus `probe_manifest_digest_addressed`).

I am **not** raising this as a finding against this diff, for two reasons. It is
pre-existing, and a `Client` decomposition is a different, larger piece of work that
would be wrong to bundle into a review-fix loop (`workflow-refactor.md`, Two Hats). But
it is worth recording that the paired-wrapper shape from §1 is what makes this axis grow
at **two methods per read** rather than one — so if `Client` is ever split, the
`_addressed` pairs are a natural seam to collapse (a read-addressing facade owning all
five pairs), not a reason to revisit the default.

**ARCH-12 (decision logic must not sit inline with `fs`/HTTP/`Command`/clock/env): no
violation.** `crates/ocx_cli/src/command/package_copy.rs:129-174` does mix branches with
`tokio::fs::create_dir_all` and `tempfile::tempdir_in`, but a command's `execute` is
composition code, which ARCH-12 exempts by name. The one piece of genuine decision logic
— which target a copy lands on — is extracted as the pure `resolve_target` at `:191-201`,
which touches no I/O. That is the rule applied, not dodged.

---

## 5. `scratch_root` — dependency direction is right; the `Option` is the weak part

**Direction: correct, and inverted the right way.** `oci/copy.rs:162` takes
`scratch_root: Option<&Path>` — a primitive. The module imports no `FileStructure`, no
`TempStore`, nothing from `ocx_cli`. The CLI reads
`context.file_structure().temp.root()` at `package_copy.rs:129` and passes the path down
(`:151`). The lower layer gained a *parameter*, not knowledge of a higher one. That is
DIP applied correctly — the alternative (having `oci/copy.rs` reach for a `TempStore`)
would have been the violation, and it was not taken.

### [Warn] The `None` arm is a documented-wrong default sitting in a production signature

`oci/copy.rs:141-144`:

> `None` falls back to `$TMPDIR`, which is a placeholder and not a choice: it is
> memory-backed on most Linux hosts, which is precisely what spooling to disk exists to
> avoid.

The API stating that its own default defeats the feature is the finding. Every `None`
call site in the tree is a test (`oci/copy.rs:706,730,758,762,789,813,839,875,928,967,1006,1045,1115,1147`;
`publisher/copy.rs:715`); the sole production call site passes `Some`
(`package_copy.rs:151`). So the `Option` exists to spare fourteen tests a
`tempfile::tempdir()`.

The justification at `publisher/copy.rs:190-195` — *"callers that have no store (tests,
and a library consumer with no OCX home)"* — is the part I checked hardest, because if
the library-consumer case is real the `Option` is earned. **It is not, as written.**
`CLAUDE.md` states plainly: *"`ocx_lib` is not a published library; the binary is the
only consumer."*

**Refutation attempted, and it half-succeeds:** `arch-principles.md:17` records that
`ocx-mirror` vendors ocx as a submodule with an `ocx_lib` path dep. So there *is* a
second in-tree consumer, and "the binary is the only consumer" is not strictly true. That
makes the case for the `Option` weaker rather than stronger, though: `ocx-mirror` is
exactly a caller that would move 100–200 MB toolchain layers, would leave an `Option`
field unset because `None` reads as "unspecified" rather than "wrong", and would then
spool through tmpfs. The documented hazard has a plausible future victim, in-tree.

**Suggested fix, cheapest form:** keep the signature but make the field non-`Option` on
`CopyRequest` and take `&Path` in `copy_leaf`; give the tests one shared helper returning
a `TempDir`. Fourteen mechanical call-site edits, and the hazard becomes unrepresentable.
If that is judged not worth the churn now, the minimum is to move the warning from the
function doc onto the **field** at `publisher/copy.rs:195`, since that is where a caller
building a `CopyRequest` will actually read it.

---

## 6. `ClientError::TraversalLimitExceeded` + `TraversalLimit` — right home

`oci/client/error.rs:86-91` (variant) and `:176-193` (enum). Correct placement, on
balance:

- `oci/copy.rs` is a peer of `client.rs` inside `oci/`, `copy_leaf` returns
  `Result<_, ClientError>`, and homing the variant there is what lets the copy engine
  keep **one** error type instead of minting a `CopyEngineError` that would need a
  `From` into `ClientError` anyway — a fifth hop for no gain.
- PKG-11 is satisfied: the variant carries `limit` and `actual` as typed fields, the
  three constants have rationale comments (`oci/copy.rs:38-52`), and `TraversalLimit`
  being a closed enum rather than a message fragment is exactly what PKG-11 asks for
  ("a caller can tell hostile input from a ceiling that wants raising without parsing
  the message"). The classification at `error.rs:302-316` puts it in `DataError` with a
  stated reason.

### [Suggest] The message hardcodes copy vocabulary into a shared taxonomy

`error.rs:85`: `"{limit_kind} limit of {limit} exceeded (reached {actual}) while copying {subject}"`.

`ClientError` is the whole `oci` transport taxonomy, but this variant can only ever be
raised by a copy. A future bounded traversal elsewhere in `oci/` — a referrer walk during
verify, say — either reports itself as "while copying" or has to add a second variant.
Dropping `while copying` and letting `subject` carry the noun costs nothing today and
keeps the variant reusable. Genuinely trivial; ignore if the shape is deliberate.

---

## Summary

| # | Finding | Severity | Location |
|---|---|---|---|
| 1 | `ReadAddressing` default inversion — right axis, right failure direction; mirror hot path structurally unforgettable | **Confirmed sound** | `oci/client.rs:139-157`, `:350-357` |
| 2 | `probe_manifest_digest_addressed` doc claims "every caller today wants a mirror"; 3 of 5 ask for `Canonical`, and that clause is the sole stated rationale for the method's divergent shape | **[Warn]** | `oci/client.rs:1990-1992` vs `package/cascade/apply.rs:205,350,404` |
| 3 | Two shapes coexist for one decision (paired-wrapper ×4, no-default ×1); both defensible, selection rule undocumented | **[Suggest]** | `oci/client.rs:139-149` |
| 4 | `push_blob_from_path` required rather than defaulted — correct; no new reachable-panic class (every `unimplemented!()` sits beside a pre-existing one, all `cfg(test)`) | **Confirmed sound** | `oci/client/transport.rs:193-212` |
| 5 | `CopyErrorKind::Registry` is the family's only `transparent` + delegated-classification arm; `SignError`/`VerifyError` carry no equivalent latent defect. Fix is correct and tested; the hazardous shape is preserved behind a doc comment | **[Suggest]** | `publisher/copy.rs:58-68,118-119` vs `sign/error.rs:257`, `verify/error.rs:515` |
| 6 | Error chain is four layers, two of them transparent, so the rendered chain understates the type graph | **[Suggest]** | `publisher/copy.rs:119` + `error.rs:95-96` |
| 7 | ARCH-01 actively applied (`Transfer`); ARCH-03 block-half holds, method-half pre-existing; ARCH-12 clean | **No finding** | `oci/copy.rs:260-269`, `oci/client.rs:186` |
| 8 | `scratch_root` direction correct (DIP), but `None` is a documented-wrong default with a plausible in-tree victim (`ocx-mirror`) | **[Warn]** | `oci/copy.rs:141-144,162`; `publisher/copy.rs:188-195` |
| 9 | `TraversalLimit` home correct, PKG-11 satisfied; message hardcodes "while copying" into a shared taxonomy | **[Suggest]** | `oci/client/error.rs:85-91,176-193` |

**No [Block] and no [High].** Two [Warn], five [Suggest], and three structural decisions
that hold up under an attempt to break them. All three of the changes the brief asked me
to judge as *designs* are the right calls; what is wrong in each case is smaller than the
decision — a false sentence in a doc comment (#2), a preserved hazard shape (#5), and an
`Option` whose `None` arm the code itself calls wrong (#8).

The two [Warn]s share a shape worth naming: in both, a doc comment carries a claim the
code contradicts, and in both the claim is the *justification* for a deliberate
divergence. That is more expensive than an ordinary stale comment, because the next
author reasons from it.


