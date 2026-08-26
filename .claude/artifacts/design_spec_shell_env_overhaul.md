# Design Contracts — Shell Environment Overhaul

Executable spine of [`adr_shell_env_overhaul.md`](./adr_shell_env_overhaul.md).
The ADR is **Accepted** for the purposes of this document; nothing here re-decides it.

**How to read.** Every contract carries `ADR:` naming the Decision(s) it derives from, and is
written so a tester can produce failing tests from the contract text alone. `ASSUMPTION:` marks a
reading chosen where the ADR was ambiguous, with its justification.
`[NEEDS CLARIFICATION: …]` marks the (few) genuinely unresolvable items.

**Corrections applied.** Seven discovery findings supersede the ADR's own wording; they are folded
into the contracts below and listed once, with their contract IDs, in §5. Where a contract differs
from the ADR text, the contract wins and says why.

---

## 1. Component Contracts

### 1.1 The ledger and its carrier

#### C-001 — `LedgerEntry`, wire field `type`
**ADR:** Decision 1 (Contract, "There is no reusable `Entry` serialization").

```rust
// crates/ocx_lib/src/shell/reconcile.rs
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LedgerEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

impl From<&crate::package::metadata::env::entry::Entry> for LedgerEntry { /* … */ }
```

Behaviour:
- The **wire field name is `type`, never `kind`** — the spelling `ocx_cli::api::data::env::EnvEntry`
  already emits and the nushell shim already parses (`$_ocx_e.type == "path"`).
- `ModifierKind` (`crates/ocx_lib/src/package/metadata/env/modifier.rs`) **gains `Deserialize`**; it
  already derives `Serialize` + `JsonSchema`. No other change to that type.
- `separator` is omitted from the wire when `None`.

Edge/error cases:
- Deserializing an unknown `type` value fails the whole `Ledger::decode` (⇒ ledger treated as absent,
  C-003). It does **not** produce a partial ledger.
- `From<&Entry>` is infallible and copies `value` **byte for byte** (see C-008, C-009).

Tests this must pass:
- Field-name parity: serializing a `LedgerEntry` and an `EnvEntry` built from the same `Entry`
  produces the same key set for `{key, value, type, separator}` — `EnvEntry`'s CLI-only
  `source: Option<EntrySource>` is the one permitted extra.
- `{"key":"PATH","value":"/a","kind":"path"}` (the wrong spelling) fails to deserialize.

**Rejected here as in the ADR:** hoisting `EnvEntry` into `ocx_lib`. Its `source` field is CLI-side
patch provenance the ledger has no use for.

**Extended by [A-08](./adr_shell_env_addenda.md)** — `separator` holds the **effective** separator: the
post-`reconcile_list_separators` value with the kind's default already applied, resolved once at record
time and never at revert time. It is therefore **always `Some` for `kind == List`**, defaulting to
`package::metadata::env::list::DEFAULT_SEPARATOR` (`" "`); `None` is reserved for path-kind and
continues to mean `env::PATH_SEPARATOR`.

---

#### C-002 — `Ledger` shape
**ADR:** Decision 1 (Contract, "Shape"), Decision 5 (`verdict`).

```rust
pub struct Ledger {
    pub v: u8,                              // schema version, inside the payload
    pub fp: String,                         // watch-set fingerprint (C-019)
    pub verdict: Option<Verdict>,           // Some(Inert) | Some(NoProject); never Some(Activate)
    pub over_cap: Vec<ScopeId>,             // scopes the cap dropped (C-004); omitted from the wire when empty
    pub scopes: Scopes,
}
pub struct Scopes { pub global: Option<Applied>, pub global_priors: Priors, pub project: Option<ProjectScope> }
pub struct ProjectScope { pub key: String, pub dir: PathBuf, pub applied: Applied, pub priors: Priors }
pub type Applied = Vec<LedgerEntry>;
pub type Priors  = BTreeMap<String, Prior>;
pub enum Prior { Unset, Value(String) }
pub enum ScopeId { Global, Project }    // wire spelling: "global" / "project"
```

Behaviour:
- `v` describes **shape**, the envelope tag describes **encoding** (C-003). A change that is both
  bumps both.
- `verdict` is the **negative cache**: only `Inert` and `NoProject` are ever written. An `Activate`
  verdict is re-derived every time and never read from the carrier — caching it would make the ledger a
  consent input, which C-007 forbids. `NoProject` (the walk resolved no project) is not consent-derived
  — there is no project to consent to — so it leaves C-007 untouched, and `project_dir` is folded into
  `fp`, so entering any project expires it. It is what makes an ordinary `cd` stat-only.
- `over_cap` names the scopes whose payload the 16 KiB cap dropped (C-004). It is an **optional
  additive field on this schema** — it bumps neither `v` nor the envelope tag — and a scope it names is
  reconciled exactly as an absent scope. Empty is the normal case and is omitted from the wire.
- **Both** scopes record priors (R1). The original justification for omitting them on the global side —
  "the global tier is the user's own file and is never *left*" (Decision 4, OQ-1) — conflated *the scope
  is never exited* with *a key is never removed from it*, and `ocx remove --global <pkg>` removes keys.
  Without a global prior, a retired global constant had nothing to revert to and ocx's value stayed in
  the shell for its whole life; and where a project constant shadowed a global one and both retired in
  one prompt, the restore wrote **global's** value back (the project prior is captured after global
  applied, C-018), losing the user's own.
- `global_priors` is a **sibling** field on `Scopes`, not a `priors` member inside `global`: turning that
  JSON array into an object would fail every live carrier's decode, which is the fleet-wide `priors` loss
  A-04 exists to forbid. As an optional additive field it bumps neither `v` nor the envelope tag, and it
  is omitted from the wire when empty.
- It is captured against the **pre-global** environment — the one place the user's own value for a global
  constant is still visible — which only the ledger's producer (`next_ledger`) sees.
- `Ledger::prior` therefore **chains**: where a project prior holds the exact value the global scope
  recorded as its own constant for that key, the lookup hops to `global_priors`. Unreachable while global
  still declares the key, because `retire_recorded_constant` returns early for a key `desired` still
  declares. A project prior that is *not* global's recorded value is the user's own and is restored
  verbatim.

`ASSUMPTION:` `Prior` is keyed by env key in a `BTreeMap`, not carried inline on `LedgerEntry`.
Justification: the ADR's shape line writes `{ key, dir, applied, priors }` as four sibling fields, and
`priors` must survive for a constant that is later retired out of `applied` (C-016) — an inline field
would be deleted with the entry it hangs off. `BTreeMap` (not `HashMap`) keeps the encoded payload
byte-stable across runs, which the fingerprint and the golden fixtures both depend on.

**Extended by [A-04](./adr_shell_env_addenda.md), [A-05](./adr_shell_env_addenda.md),
[A-13](./adr_shell_env_addenda.md)** — `v` is additive-only, and a shape break ships a `v-1`
revert-read arm in the same release (A-04); prior capture reads set-ness via `std::env::var_os`, so a
set-but-empty variable is `Value("")` and never `Unset` (A-05); `fp` folds the raw `OCX_CONSENT_*`
values, and the watch set it covers gains the ledger-recorded config-tier paths plus the project's
consent stamp (A-13).

---

#### C-003 — `Ledger::decode` and the `<tag>.<payload>` envelope
**ADR:** Decision 1 ("Envelope tag — outside the payload", Degradation rule).

```rust
pub fn decode(raw: &str) -> Option<Ledger>;
```

Grammar of `__OCX_ENV_STATE`: `<tag> "." <payload>` where
- `<tag>` is **one ASCII digit** naming the encoder,
- `.` is the separator (absent from the base64url alphabet, so the split is unambiguous and needs no
  length prefix),
- `<payload>` is the encoded body.

Encoder `1` = base64url(compact JSON), **uncompressed**, and is the only encoder this design defines.

Returns `None` (⇒ "treat as absent", C-006) for **every** failure, with no distinction at the type
level: unrecognised tag, missing `.`, tag not a single digit, payload not valid base64url, payload not
valid JSON, JSON not matching the schema, unrecognised `v`, or the raw value exceeding 16 KiB.

Edge cases that must each be a test:
- `""`, `"1"`, `"1."`, `".abc"`, `"2.<valid-payload-for-1>"`, `"11.abc"`, `"x.abc"` → `None`.
- A valid `1.<payload>` whose JSON carries `"v": 99` → `None`.
- A valid payload truncated at any byte → `None`, never a panic.

**Forward-compat consequence (normative):** an older binary seeing `2.…` does not know the tag, so it
repairs rather than misreads. That is what lets a future encoder `2` (deflate) ship with no migration
and no dual-read window. The trigger for introducing encoder `2` is named, not left to taste: the NFR
benchmark's encoded ledger for a real project exceeding **4 KiB**.

**Extended by [A-04](./adr_shell_env_addenda.md)** — the new-binary-reads-old direction is not
symmetric with this one: if a shape break is ever unavoidable, the releasing binary ships a
**revert-read-only** arm for `v-1` in the same release (deserialize `applied` and `priors` only, run
one final revert/retirement pass, re-emit in the new shape). It never writes the old shape and there is
no dual-write window; losing live `priors` fleet-wide on a `self update` is the outcome that arm exists
to prevent.

---

#### C-004 — `Ledger::encode`, the 16 KiB cap, and the over-cap marker
**ADR:** Decision 1 ("Over cap ⇒ absent"), as corrected by [A-01](./adr_shell_env_addenda.md).

```rust
pub fn encode(&self) -> Option<String>;
```

- Returns `Some("1." + base64url(compact_json))` when the **whole value** is ≤ 16 KiB.
- **Over cap, `encode` MUST NOT omit the variable.** It emits a **marker-only ledger** —
  `{ v, fp, verdict, over_cap: [<scopes>] }`, both scope payloads dropped, roughly 60 bytes — which
  still encodes, still carries the fingerprint, and still decodes (C-003).
- Returns `None` **only** if even the marker fails to encode (unreachable in practice). `None` means
  **omit the variable entirely** — the second rung and the last one. There is no third: no partial
  payload, no `priors`-dropping rung, no `applied`-dropping rung.
- A scope named in `over_cap` is reconciled **exactly as an absent scope** (C-006): rebuild D from
  truth, run the subtractive prefix repair (C-016), leave constants in place.
- The caller emits **one abandonment line per transition into the over-cap state**, never per prompt,
  naming the scope whose ledger was abandoned — deferred to the first `--reconcile` run like every
  other diagnostic (C-051). `ocx shell state` reports the state **from the marker** (C-050).

**Why omission is not the first rung, stated so it is not re-proposed:** `fp` lives *inside* the
payload, so omitting the variable destroys the fingerprint too — the next prompt sees no recorded `fp`,
recomposes, re-overflows, omits and reports again, every prompt, for the life of the shell. That is
worse than the information loss it was meant to signal, and it breaks the stat-only no-op budget
(C-019, C-044). This is **one rule, not a ladder**: the information lost is exactly what "omitted
entirely" already lost; the fingerprint is what is recovered.

Test: build a ledger with N synthetic entries until the full payload exceeds 16 KiB; assert the emitted
script **does** set `__OCX_ENV_STATE`, that the value **decodes** with `fp` intact, no `applied`, and
`over_cap` naming the scope. **Named red state:** a build that omits the variable fails an assertion of
**zero** recomposes over five further prompts with a static watch set.

---

#### C-005 — `Ledger::empty`
**ADR:** Decision 1 (Component Contracts row).

```rust
pub fn empty() -> Ledger;   // v = current, fp = "", verdict = None, scopes all None
```

Required because `decode` returns `Option` and `plan` takes `&Ledger` — without it the absent-ledger
call is unrepresentable. `Ledger::empty()` is what the first prompt of a shell plans against.

---

#### C-006 — Degradation rule; absent vs corrupt is normative
**ADR:** Decision 1 (Degradation rule).

Both an absent ledger and a corrupt one take **the same code path** — rebuild D from truth, repair
lists **by the retirement rule** (C-016, subtractive), leave constants in place — but they are **not
the same situation**, and the distinction is normative:

| Situation | Detection | Repair actually runs? | Output |
|---|---|---|---|
| **Absent** — first prompt of a shell | `__OCX_ENV_STATE` unset | No: nothing was applied, so nothing is stranded | Nothing. Log at **debug**. |
| **Corrupt** — decode returned `None` on a non-empty value | variable set, `decode` → `None` | Yes: a scope *was* applied and its record is gone | **One** line naming the repair. Log at **debug**. |
| **Over cap** — the payload exceeded 16 KiB | the decoded **marker**: `over_cap` names the scope (C-004) | Yes: the scope *was* applied and its record was dropped | **One** line naming the abandoned scope, per transition into the state, not per prompt (C-004). |

Hard rules:
- Never `warn!` — an absent ledger is the normal first-prompt case and this repo forbids warning on a
  common benign state.
- Never direnv's hard refuse. A reconciler that can brick a prompt on a bit flip is worse than a stale
  prompt (D3).
- Constants are **never guess-unset** during repair. A repaired session leaves `JAVA_HOME` as it found
  it.

---

#### C-007 — The three normative forgery rules on the carrier
**ADR:** Decision 1 ("Three normative rules replace the claim", "What forging it is worth").

The carrier is **untrusted input**. Its only permitted effects are (i) naming the revert set and
(ii) supplying the equality operand for the exit guard.

- **(a) `key` and `dir` are advisory identity labels.** Both are re-derived from the CWD walk every
  prompt. They MUST NOT construct a path, MUST NOT re-grant consent, and **MUST NOT gate a revert**. A
  `dir` equal to the walk's result means *same scope, no scope event*; **any** other value — including
  a malformed, absent or forged one — means the recorded scope has been **left**, so
  `L.scopes.project.applied` becomes the **revert set** and is executed before the newly discovered
  scope is applied (a switch is revert-then-apply in one pass, C-018). The revert set is **L-scoped,
  always**, and is never intersected with D (C-017): a constant in L that D no longer names is
  reverted under the `C == L.applied` guard, never discarded. `applied` entries are self-describing
  (key, value, kind), so `dir` is not needed to undo them.
- **(b) `kind` and `separator` are re-derived from D** for every key present in D. L's copies are used
  only for the revert set. A `priors` restore **never runs for a key D currently declares list-kind**.
- **(c) An L entry may only ever undo itself.** For a constant: restore its own recorded prior under
  the `C == L.applied` guard. For a list: remove the elements it itself records. It never selects a
  value for a key it is not reverting, never overrides the kind D declares for a key D declares, and
  never names a path.

Key-set containment: **ocx touches exactly `D ∪ L`.** L is bounded because ocx only ever writes into
it keys it itself composed from D.

Required tests (each must red on a deliberate rule removal):
- Forged `kind`: `PATH` forged as `Constant` with `priors["PATH"] = Value("/attacker/bin")` and
  `applied["PATH"].value` set to the shell's real `PATH`. Assert leaving the project does **not** set
  `PATH=/attacker/bin` — rule (b) re-derives `PATH` as list-kind from D and refuses the restore.
- Forged `key = "../../.."`: assert no path is ever constructed from a ledger key; the sweep and stamp
  paths are built from the CWD walk only.
- Forged `dir` pointing elsewhere: assert the recorded project scope is **reverted** — its `applied`
  map becomes the revert set — never discarded, that no path is constructed from `dir`, and that the
  global scope is unaffected. Red state: a `plan` that voids the scope on a mismatch leaves the
  project's `JAVA_HOME` set after the switch.

**Not** a test target: authenticating the carrier. Setting `__OCX_ENV_STATE` requires the ability to
set an arbitrary variable in the victim's shell, which is the ability to set `PATH` directly. The
carrier is not a privilege boundary; the three rules exist so a **bug in the ledger's producer** cannot
become a value-selection primitive.

**Extended by [A-02](./adr_shell_env_addenda.md), [A-06](./adr_shell_env_addenda.md)** — `plan` refuses
`ModifierKind::Constant` for `PATH` and `PATHEXT`, compared through `EnvKey` so the refusal is
case-insensitive on Windows, and decode discards any ledger recording either key as `Constant` or
carrying `priors` for either; rule (b) alone has no operand when D names the key nowhere (A-02). The
forgery posture holds only among **equal principals**: across a privilege crossing such as `sudo -E`,
rule (c)'s constant arm is an **arbitrary-value-for-arbitrary-key** primitive (`LD_PRELOAD`,
`PYTHONPATH`, `GIT_SSH_COMMAND`), not removal-only, because the revert set is L-scoped and the forger
authors both operands of the guard — the named mitigations are `sudo`'s `env_reset` / `secure_path`
defaults, `unset __OCX_ENV_STATE` in the privileged shell, and `ocx shell state` (A-06).

---

#### C-008 — Invariant L-1: literal values, never references
**ADR:** Decision 1 (Invariant L-1).

Every `applied` value and every `priors` value is **the exact string ocx wrote, or the exact string it
found, byte for byte**. The ledger never stores a *reference* that must be re-interpreted at revert
time: not a package digest to re-resolve, not an `ocx.toml` key to re-read, not a template to
re-interpolate.

In one line: **anything the revert path needs must already be in the ledger as a literal string.**

Consequences that are testable:
- The revert path performs **zero** store I/O and **zero** config reads. A test that deletes the
  package directory (simulating `ocx clean`) and then leaves the project must still revert exactly.
- A test that mutates `ocx.toml` between apply and revert must observe the revert restoring the
  **pre-apply** value, not a value re-derived from the new file.

**Extended by [A-19](./adr_shell_env_addenda.md)** — the ledger stores the exact bytes ocx wrote, and
the **comparison** is what is normalised, never the stored string: a PATH element is compared
segment-exact after stripping a single surrounding pair of `"`, case-sensitively on Unix and
ASCII-case-insensitively on Windows. So `C == L.applied` is evaluated with that predicate, **not with
`==`** — normalising the stored string would break this invariant and make the constant-restore path
emit a wrongly-cased directory.

---

#### C-009 — Invariant L-2: raw, unescaped; escaping is emit-time per arm
**ADR:** Decision 1 (Invariant L-2), Component Contracts (`Shell` row).

The payload holds keys, values and kinds. **It never holds shell text.** Escaping happens only at emit
time, through that arm's escaper:

| Arm | Escaper |
|---|---|
| bash, zsh, ash, ksh, dash | `escape_posix_single_quoted` (`shell.rs:550`) |
| PowerShell, elvish | `escape_single_quoted_doubled` (`shell.rs:538`) |
| fish, nushell | `escape_value` (`shell.rs:476`) |

Why it is load-bearing: a pre-escaped value leaking into the ledger would be **double-escaped** by an
inheriting shell and single-escaped correctly by none — a silent, per-value failure. It also keeps L's
stored value directly comparable to C for the `C == L.applied` exit guard, which reads the shell's own
*unescaped* value.

Required tests:
- Round-trip every fixture value through `encode`/`decode` and assert **byte equality** with the raw
  input.
- Assert no arm's escaper output can reach `encode` (a structural guard over the reconcile module plus
  a behavioural round-trip on a value containing `'`, `"`, `` ` ``, `$`, `\`, `%VAR%`, a newline).

**Extended by [A-15](./adr_shell_env_addenda.md), [A-16](./adr_shell_env_addenda.md),
[A-20](./adr_shell_env_addenda.md)** — the table above governs the **element-match / `export_path`**
emits; for `export_constant` it is a **change, not a description**. `Shell::export_constant`
(`shell.rs:427-441`) moves to that arm's own escaper and quoting context: `escape_posix_single_quoted`
for ash/ksh/dash/bash/zsh (`export {key}='{value}'`), `escape_single_quoted_doubled` for PowerShell
(`$env:{key}='{value}'`) and elvish (`set E:{key} = '{value}'`), `escape_value` unchanged for fish,
nushell and Batch — and `escape_value`'s now-callerless `Ash | Ksh | Dash | Bash | Zsh` arm
(`shell.rs:484-489`), its `!` → `\!` replacement and the tests locking it in are **deleted** (A-15).
`escape_value`'s nushell arm reduces to `\` → `\\` and `"` → `\"` only, the `$`/`(`/`)` replacements
deleted, and its stale doc comment rewritten (A-16); its Batch arm reduces to `%` → `%%`, the
`^`/`&`/`<`/`>`/`|` caret escapes deleted as over-escaping inside `SET "KEY=…"` (A-20). **No second
escaper is minted anywhere in the reconciler.**

---

#### C-010 — `plan`
**ADR:** Decision 3 (whole decision), Decision 1 (degradation), Component Contracts row.

```rust
pub fn plan(
    desired: &[Entry],
    current: &Env,
    ledger: &Ledger,
    owned_prefixes: &[&Path],
) -> Plan;
```

- **Pure**: no I/O, no clock, no env reads beyond `current`, platform-neutral, unit-testable.
- `owned_prefixes` is **required, not optional**. Both the degradation rule (C-006) and the ownership
  rule (C-016) make `plan` responsible for repairing lists when L is lost, which needs `$OCX_HOME`
  (and, if [#189](https://github.com/ocx-sh/ocx/issues/189) ever lands, `.ocx/toolchain/`, additively).
  Without it `plan` either leaves every element — unbounded PATH growth across project switches — or
  reads the env itself, breaking "no I/O".
- Called **once per scope-stack pass**, producing one `Plan` covering both scopes in emission order
  (C-018).

**Extended by [A-09](./adr_shell_env_addenda.md), [A-10](./adr_shell_env_addenda.md)** — retirement
enumerates the segments **as they appear in C**, selects the prefix-owned ones absent from D, and names
that observed segment verbatim in the removal, so selection and removal share one byte-exact operand
(A-09). And before anything reaches `Plan` or `L`, `plan` drops four classes, each with one warn-once
line: a key failing `env::is_valid_env_key`; a path-kind value containing `env::PATH_SEPARATOR`; an
empty list or path element; an element containing LF or CR. `L ⊆ emittable(D)` is thereby an
**invariant, not an accident** — every emitter returns `None` for an invalid key, so without the gate L
would carry a key no arm can ever remove. `ocx run` / `ocx exec` composition is untouched: the gate is
reconciler-local (A-10).

---

#### C-011 — `Plan` and its JSON wire shape
**ADR:** Decision 3, Decision 6(b).

```rust
pub struct Plan {
    pub sets:     Vec<Entry>,                              // apply (constants + list prepends)
    pub removes:  Vec<(String, String, Option<String>)>,   // (key, element, separator)
    pub restores: Vec<(String, Option<String>)>,           // (key, prior) — None = unset it
}
```

- `removes` carries the separator per element — the whole point of C-014's signature change.
- `Plan` **has a JSON wire shape**, because nushell consumes structured data rather than emitted text
  (C-050): `{"sets":[…LedgerEntry-shaped…],"removes":[[key,element,sep|null],…],"restores":[[key,value|null],…]}`.
  The `sets` element shape reuses `LedgerEntry`'s spelling, `type` included.
- Per-shell **rendering** stays in `Shell`/`emit_lines`. `Plan` never contains shell text (C-009).

`ASSUMPTION:` `restores` carries `Option<String>` rather than a `Prior` enum on the wire.
Justification: the ADR's own Component Contracts row spells it `restores: Vec<(String, Option<String>)>`,
and `None`/`null` is the JSON-natural spelling of `Prior::Unset` for the nushell consumer.

**Extended by [A-23](./adr_shell_env_addenda.md)** — the `Plan` JSON wire shape carries a top-level
`"v": 1`, on the same envelope discipline as the ledger. `v` is **structural only**: it bumps on a
breaking reshape, never on an added field, and unknown fields inside a recognised `v` are ignored. The
consumer applies one rule — **`v` absent or unrecognised ⇒ apply nothing this prompt and return
silently**, no error and no partial apply (C-048).

---

#### C-012 — The repair gesture: `unset __OCX_ENV_STATE`
**ADR:** Decision 1 ("The repair gesture … and there is no command for it").

**No new command, no new code path.** Clearing the variable makes the next prompt see an **absent**
ledger, which C-006 already specifies in full. Because the fingerprint lives *inside* the ledger,
clearing it also forces recomputation, so one gesture covers both failure modes a user can hit:
detection went stale (the mtime+size ceiling, C-019) and the ledger is wrong.

Contracted costs, which the docs must state:
- `priors` are destroyed with the ledger. Constants can no longer be restored when the scope is later
  left — after the gesture `JAVA_HOME` keeps the project's value for the rest of that shell's life.
- **A new shell is the clean floor** and the better advice whenever one is cheap: a new shell has no
  priors, so it loses nothing.
- The gesture is **silent by construction** — the hook cannot distinguish a deliberately-cleared ledger
  from the ordinary first-prompt absence. `ocx shell state` (C-052) is what confirms it worked.
- Documenting the gesture makes `__OCX_ENV_STATE` a **user-facing contract**: it can no longer be
  renamed freely. Accepted; the name already sits in the reserved `__OCX_*` namespace that
  `is_reserved_ocx_key` gates and `ocx-sdk-python` rejects at exit 64.

Nothing here needs a test the degradation rule (C-006) does not already have.

---

### 1.2 Reconciler semantics

#### C-013 — Apply is routed per kind; list-kind is element operations only
**ADR:** Decision 3 (List-kind bullet), as corrected by [A-07](./adr_shell_env_addenda.md).

- **Apply — routed by kind, never by one rule:**

  | Kind | In-process | Emitted | Position |
  |---|---|---|---|
  | path | `utility::path::move_to_front` | `Shell::export_path` | **front**, in order |
  | list | `utility::list::append_unique` | `Shell::export_list(key, value, effective_sep)` | **back**, the whole opaque contribution, never split into elements |
  | constant | — | `Shell::export_constant` | overwrite |

  Each pair is byte-identical between in-process and emitted forms (enforced by C-021).
  **"In front, in order" is false for list-kind**: `ModifierKind::List` appends — verified at
  `crates/ocx_lib/src/package/metadata/env/list.rs:16-21` (*"appended … re-applying moves the
  contribution to the back … The consumer resolving duplicates last-wins is what the direction
  serves"*) and `crates/ocx_lib/src/utility/list.rs:55`. Emission order stays **global first, project
  second for all three kinds** (C-018) — do **not** reverse it for lists. Project still beats global
  for list-kind, but because list consumers are last-wins and project is applied **last**; reversing
  emission order would invert precedence and break parity with `apply_entries`, which `ocx run` uses.
- **Revert**: **remove our elements**, never restore an old string — for list-kind that "element" is
  the **whole opaque contribution**, removed flank-delimited with the effective separator (C-014).
  Removal commutes with foreign prepends and appends. Delete-if-found; **absence is not an error**.
- **Ownership**: elements under `$OCX_HOME` are self-identifying by prefix; arbitrary elements (project
  `[env]` additions) are identified only by L naming them. The prefix test doubles as the repair path
  when L is lost.
- Byte-snapshot restore (conda's `OCX_PATH_BACKUP` shape) and direnv's untyped diff are both **rejected**
  — they clobber every foreign PATH edit since activation.

---

#### C-014 — `Shell::remove_list_element`
**ADR:** Decision 3 (List-kind, "This needs one new primitive"), Component Contracts (`Shell` row).

```rust
// crates/ocx_lib/src/shell.rs — beside export_path (:154) / export_list (:319)
pub fn remove_list_element(
    self,
    key: impl AsRef<str>,
    value: impl AsRef<str>,
    separator: Option<&str>,
) -> Option<String>;
```

- The name is `remove_list_element`, **not** `remove_path_element`, and the **separator parameter is
  mandatory to the contract**. Without it every non-default-separator list var is permanently
  unrevertible: `CFLAGS` as `{type = list, separator = " "}` or `CLASSPATH` as `":"` on Windows applies
  through `export_list` (which *does* take a separator) and would then remove nothing, or split on the
  wrong byte and corrupt the value.
- `separator: None` means **the platform PATH separator**, mirroring `export_path`/`export_list`
  exactly — so PATH keeps its shipped spelling and list vars get a correct one.
- Returns `None` for an **invalid env key** (delegating to `env::is_valid_env_key`, same as
  `export_path`) **or** for `Shell::Batch`.
- Per-entry separators are settled upstream by `env::reconcile_list_separators`; this primitive never
  guesses one.

**Batch rationale, corrected (carry this, not the older wording):** it is *not* "cannot express it" —
`export_path` **does** delete an element on Batch via `%VAR:search=%`. The reason is that `cmd.exe`'s
only substring-replace primitive is **case-insensitive with no case-sensitive form**, and list elements
need case-sensitive matching (this is `export_list`'s actual reason too). Batch also hosts no prompt
hook, so nothing consumes it.

**Load-bearing arms** — the six hook shells: **bash, zsh, fish, PowerShell, nushell, elvish** (elvish's
arm and its narrower guard are amended into C-043 below). The strict-POSIX trio (ash/ksh/dash) get the
primitive because `export_path` already has it and asymmetry is the bug; they ride the existing
`export_path` template.

**Escaping is per arm** (C-009). Routing every arm through `escape_value` would ship a shell injection:
it is the **double-quoted**-context escaper and deliberately leaves `'` untouched, so a PATH element
like `/tmp/a';id;'b` — reachable from a project `[env]` value — would execute at every prompt.

**Hazards to carry, not re-derive** (already closed in shipped code):
- zsh glob over-match and bash `${//}` pattern escaping — both handled by quoting the expansion inside
  the pattern (`${KEY//:"$__ocx_p":/:}`, `shell.rs:174-176`).
- The strict-POSIX arms hand the value to `awk` through `ENVIRON`, never `-v` (which decodes backslash
  escapes and breaks the byte-exact compare).

**Hazards that are new and externally documented:**
- **PowerShell substring over-match** — a naive `-notlike`/substring removal that strips `C:\WINDOWS`
  also strips `C:\WINDOWS\system32`. Matching must be **segment-exact**.
- **PowerShell env-var name case** — `$env:PATH` and `$env:Path` are the *same* variable on Windows and
  *different* variables on Linux/macOS ([PowerShell#3571](https://github.com/PowerShell/PowerShell/issues/3571)).
  Platform-conditional casing, not a scalar op.
- **fish has no remove primitive** — the field workaround is index-based `set -e VAR[N]`, and removing
  2+ elements in one call shifts every later index. Removal goes **highest-index-first**, or re-resolves
  between removals ([fish-shell#8604](https://github.com/fish-shell/fish-shell/issues/8604)).

**Extended by [A-08](./adr_shell_env_addenda.md), [A-10](./adr_shell_env_addenda.md),
[A-19](./adr_shell_env_addenda.md), [A-20](./adr_shell_env_addenda.md)** — it is `append_unique`'s
inverse: **flank-delimited removal of one whole contribution, never a segment op**, and a list-kind
revert always passes `Some(effective_separator)`, `None` being path-kind only (A-08). `plan` never
hands it a key or element no arm can emit, so absence-is-not-an-error is the only degradation left
(A-10). **One comparison rule across all three implementations**: segment-exact after stripping a
single surrounding pair of `"`, ordinal on Unix and `OrdinalIgnoreCase` on Windows, each arm selecting
at emit time under `cfg!(windows)` — the PowerShell arm replaces `-ne` with
`[String]::Equals($_, $__ocx_p, [StringComparison]::Ordinal|OrdinalIgnoreCase)` and trims one leading
and one trailing `"` per split segment; the emitted **key** is never re-cased (A-19). Batch's `None` is
joined by three further refusals on `export_path`/`export_constant` (`%`, LF, CR) and a documented
delayed-expansion-off precondition (A-20).

---

#### C-015 — Apply/revert semantics
**ADR:** Decision 3 (Constant-kind bullets; the path/list apply gate).

Five rules, all five testable in isolation. Rules 2–4 are constant-only; rule 0 is the
path/list half of rule 1.

0. **Path/list apply gate `fold(D, C) == C`.** Emit nothing for a path or list key where
   applying it would change nothing, decided **per key and all-or-nothing** over the
   entries contributing to that key. The operand is the *live environment*, not the
   ledger: fold D into a copy of C with the same `move_to_front` / `append_unique` the
   emitted arms are contracted to equal byte for byte, and settle the key iff its value
   came back identical. Never re-derive the ordering rule — a second copy drifts.
   Rationale (do not weaken): path/list applications are idempotent, so re-emitting is
   *correct* but not free — the emitted form is in-shell `PATH` string surgery, measured
   (re-measured 2026-08-25, `bash --norc`, 500 evals per stream) at **~0.28 ms per prompt**
   for 7 apply lines over a 46-segment `PATH` (7-segment `PATH`: 0.222 ms; 120-segment:
   0.363 ms), scaling with (entries × segments), forever. Without this rule the reconciler
   has **no fixed point**
   and D2's "every prompt re-converges" is factually "every prompt re-applies"
   ([ocx-sh/ocx#342](https://github.com/ocx-sh/ocx/issues/342)). A key any scope declares
   constant-kind is excluded and falls to rule 1 — settling *it* from C would re-assert
   ocx's value over a mid-session override, the exact failure rule 1 exists to prevent.
   Widening `plan`'s inputs to include C for path kinds is deliberate and costs the
   lost-ledger repair (C-006) nothing: the repair path reads C directly, and a key whose
   fold is already live needs no repair by definition.
1. **Constant apply gate `D ≠ L`.** Set to D **only where the composed value actually changed**. Where
   `D == L`, ocx has already written this value and leaves C alone. Rationale (do not weaken):
   "set to D unconditionally" makes mid-session-override protection one-sided — a manual
   `export JAVA_HOME=…` would survive *leaving* the project but be clobbered by any recompose at the
   next prompt, which fires every prompt rather than once. mise shipped exactly that in 2026.8.0 and
   reversed it in 2026.8.9 ([jdx/mise#12094](https://github.com/jdx/mise/issues/12094)).
2. **Exit guard `C == L.applied`.** Restore the prior only if the current value is still what ocx
   wrote; otherwise leave C. Never clobber a user's mid-session override on the way out.
3. **Prior re-capture on overwrite.** At apply time, `C ≠ L.applied` ⇒ `L.prior := C`. Without this the
   `D ≠ L` gate is not enough: on a genuine `[env]` change `L.applied` becomes D while `L.prior` still
   holds entry-time state, so `C == L` again and ocx **unsets** on the way out a variable the user set
   by hand — strictly worse than direnv restoring a stale one.
4. **Coincidence (`C ≠ L` but `C == D`).** Claim silently; `prior := C`. Accepted leak, stated: the
   prior is now D, so leaving restores D rather than removing it. The user typed that value; restoring
   what they typed beats unsetting it.

**Which trigger dominates.** PWD events (enter / leave / switch) and fingerprint changes are the *only*
two things that run apply. A same-project prompt with an unchanged fingerprint runs neither. So a
foreign write to a constant in D **survives every prompt until the project's own configuration or the
binary changes** — at which point rule 1 fires and rule 3 re-captures.

**Only one sense of "unset" ships**: `priors: Unset`, meaning *"the variable did not exist before ocx
set it"*, so reverting **removes** it. **Desired-unset** — a project asking for an *ambient* variable to
be removed — is [#265](https://github.com/ocx-sh/ocx/issues/265), **deferred and not pulled in**. It is
a package-metadata wire-format change (a fourth `ModifierKind` variant) plus a project-config-syntax
change; D6 keeps both out of scope. No work package may add a `ModifierKind` variant.

**Extended by [A-05](./adr_shell_env_addenda.md)** — prior capture MUST use `std::env::var_os(key)`:
`None` ⇒ `priors: Unset`, `Some(s)` ⇒ `priors: Value(s)`, **including `Some("")`**. Capture reads
set-ness, never truthiness — no `filter(|v| !v.is_empty())` and no `unwrap_or_default` anywhere on that
path — and reverting `Value("")` emits that arm's `export_constant(key, "")`, never `Shell::unset`.
Named residual: on Windows `cmd.exe`'s `SET "K="` and PowerShell's `$env:K = ''` both delete the
variable, so `Value("")` and `Unset` collapse there — asserted at tier 1 on the emitted string, not on
the runtime effect.

---

#### C-016 — The retirement rule: one rule, two triggers, subtractive list repair
**ADR:** Decision 3 (Retirement).

**On every recompose, retire what ocx owns and D no longer wants.**

- **Ownership** is either **recorded** (L names the element as applied) **or structural** (the element
  sits under `$OCX_HOME`; plus `.ocx/toolchain/`, additively, if #189 ever lands).
- **Lists — subtractive, and the wording is load-bearing:** *remove every prefix-owned element that is
  not in D.* Never merely "ensure D's elements are present, in front". The additive reading is what
  leaves both `…/packages/<old>/bin` and `…/packages/<new>/bin` on PATH after a digest change: they are
  *different strings*, so `move_to_front` does not dedupe them — it reorders the new one to the front of
  a list that still contains the old one, and any later foreign prepend or second recompose can put the
  stale one back in front.
- **Arbitrary (non-prefix-owned) elements** are retired only where L records them — the same bound as
  everywhere else, since nothing else identifies them as ours.
- **Constants** take the same shape: a constant in L that D no longer declares is retired by restoring
  its recorded prior, under the existing `C == L.applied` guard (C-015 rule 2).
- **Two triggers, one code path**: **scope exit** (D for that scope becomes empty) and **recompose in
  place** (D changed under a live scope). They are the same code, not two rules that agree.

The three real-world cases this rule and only this rule covers: `ocx remove --global` from another
shell, a branch switch that deletes a tool from `ocx.lock`, and a package digest bump.

**Fault injection for the red state:** make the list repair additive and watch the digest-duplicate
assertion (S-010) go red.

**Extended by [A-09](./adr_shell_env_addenda.md)** — structural ownership is **component-wise**:
`Path::starts_with` against an `$OCX_HOME` canonicalized once at reconcile start, a component boundary
and never a byte prefix, so `/home/u/.ocx-backup/bin` and `/home/u/.ocxevil/bin` are **foreign** and
survive a corrupt-carrier repair untouched. `plan` already receives `owned_prefixes: &[&Path]`, so the
prefixes are `Path`s at the seam.

---

#### C-017 — The revert set is scoped to L, never intersected with D
**ADR:** Decision 3 ("The revert set is scoped to L"), Decision 1 rule (c).

The revert set for scope S is **exactly** the keys `L.scopes[S].applied` names, each undone by the rule
for its own kind. L alone suffices to act.

The superseded rule — *"an L entry whose key is in neither D nor the prefix-owned set is discarded"* —
was a **real defect**, not a tightening: on `cd` out of a project, a project constant is by definition
no longer in D (that is what leaving means) and is not prefix-owned either, so the old rule **discarded
`JAVA_HOME` instead of reverting it** and leaked the project's value into the rest of the session.

Containment comes from **`D ∪ L`** (C-007), not from D.

**Keys outside `D ∪ L` are never read or written.** Foreign variables are structurally invisible — the
whole-env-capture bug direnv has ([#82](https://github.com/direnv/direnv/issues/82),
[#1249](https://github.com/direnv/direnv/issues/1249)), removed by construction rather than by care.

**Extended by [A-03](./adr_shell_env_addenda.md)** — `dir` never gates this revert either: **any** `dir`
other than the walk's own result — malformed and absent included — means the recorded scope has been
**left**, so `L.scopes.project.applied` becomes the revert set and runs before the newly discovered
scope is applied (C-007a). Every project switch is a `dir` mismatch; that is what a switch *is*.

---

#### C-018 — Scope stack: emission order, prior-capture ordering, one `project` slot
**ADR:** Decision 3 (Scope stack, Consequence for revert, "Why a single `project` slot is enough").

**Emit and apply global first, project second.** Stated in the composer's own vocabulary because
conflating emission order with resolved order is a known bug class here (`composer.rs:1077-1085`:
*"the **last** entry pushed is **first** in the resolved PATH"*).

- Constants: later write wins ⇒ project overrides global.
- **Path-kind**: each application prepends ⇒ the later prepend lands earlier ⇒ project in front;
  resolved PATH order is project → global.
- **List-kind**: each application **appends** ⇒ the later append lands **last** ⇒ project last, which
  *is* project winning, because list consumers resolve duplicates last-wins (C-013).
- All three kinds agree on the outcome: **project beats global** — by three different mechanisms.
- Resolution isolation per `adr_global_toolchain_tier.md` is untouched — this is shell-level
  concatenation; global still never composes *into* project resolution.

**Prior capture is normatively AFTER global's apply.** The ordering is: apply global → capture priors
for the project's keys → apply project. A constant prior captured at project entry therefore holds
**global's** value, which is correct: reverting the project restores global's value and leaves the
global scope intact. Capturing before global's apply would silently tear down global's constants when
leaving a project that never owned them.

**Two cases, kept apart:**
- *Scope exit* — leaving a project reverts the **project** section only; global is untouched.
- *Recompose in place* — the global lock or `$OCX_HOME/ocx.toml` changed while the scope stayed active;
  the retirement rule (C-016) then retires global elements and global constants **mid-session, in the
  same shell, at the next prompt**. `ocx remove --global foo` in another terminal is exactly this, and
  it must work.

No scope-exit rule forbids mid-session global retirement. What never happens mid-session is a
*wholesale* teardown of the global section.

**Exactly two slots — `global` and `project`.** Three verified facts, not an assumption:
1. `ConfigLoader::project_path` tier 3 walks up for the **nearest** `ocx.toml` and returns on the first
   hit, so a project nested inside a project does **not** layer — the inner one *replaces* the outer,
   and moving between them is a **switch** (revert A, apply B in one pass).
2. There is **no implicit `$OCX_HOME/ocx.toml` fallback** — a CWD-walk miss is a hard `None`; the global
   tier is reachable only through `--global` / `OCX_GLOBAL`.
3. The explicit tiers (`--project`, `OCX_PROJECT`) resolve one file and return, also replacing.

A ledger shaped for a deeper stack would carry a case nothing can produce.

**Extended by [A-07](./adr_shell_env_addenda.md)** — emission order is **global first, project second
for all three kinds**, and must not be reversed for list-kind: project precedence there is delivered by
*last* position into a last-wins consumer, not by front position. Reversing the order would invert
precedence and break parity with `apply_entries`.

---

#### C-019 — Fingerprint, watch set, mtime+size fast path, and its named ceiling
**ADR:** Decision 3 (Fingerprint / fast path).

**Watch set** (exactly these members):
1. project `ocx.toml`
2. project `ocx.lock`
3. `$OCX_HOME/ocx.toml`
4. `$OCX_HOME/ocx.lock`
5. the managed-config snapshot
6. the ocx binary version
7. the project directory
8. the **config-tier paths recorded in the ledger at compose time** — home `config.toml`, user
   `config.toml`, and the `OCX_CONFIG` / `--config` override if one was in effect — each with
   **presence**, mtime and size, so a tier file that did *not* exist becomes a change when created
9. the project's consent stamp, `state/projects/<key>/consent.json`

`[env]` applies on its own authority independently of the lock, so watching locks alone would miss
`[env]`-only edits.

**Elvish consults none of this.** Its per-prompt guard carries no watch-set term at all — elvish 0.21
has no in-shell mtime and no clock — so this fingerprint definition does not gate that arm; see C-043
for elvish's own two-term guard (carrier-empty OR `$pwd` changed).

**Members 8 and 9 are what make the `verdict: "inert"` cache (C-042) expirable.** Without them the
cache is unexpirable and a grant added from another terminal never takes effect until the shell
restarts. The raw values of `OCX_CONSENT_PATHS` and `OCX_CONSENT_NAMESPACES` fold into `fp` directly —
two `getenv`s, no I/O. **The per-prompt path stats the *recorded* list and reads no config**: path
discovery happened during the last `ConfigLoader` pass, which is what keeps C-042's zero-config rule
absolute — stat'ing is not parsing, and `ocx.toml` is already stat'ed on the identical path. The added
members join the same shell-side newer-than short-circuit as the existing ones (C-044). Cost: ~3-4
stats and 2 `getenv`s on a path already doing ~6.

**Fast path**: **mtime + size only**. Content hashes are computed **strictly downstream** of an
mtime/size change deciding recomposition is plausible.

**Named ceiling — stated granularity-free, not discovered in the field:** *an unchanged `(mtime, size)`
pair is invisible*, and stays invisible until something else in the watch set moves. The fingerprint
MUST compare the **full `std::fs::Metadata::modified()` `SystemTime`, never a seconds-truncated
value**, so the window is the filesystem's own granularity and nothing coarser — reading the full
`SystemTime` is free and strictly narrows it on ext4/NTFS/APFS. **FAT/exFAT (2 s) and NFS (1 s) widen
it**, and Windows is a first-class host for both. The content hash cannot rescue it, because it is
computed downstream of the mtime/size verdict by design. In practice a `git checkout` bumps mtime, so
the case is narrow. direnv and mise sit on the identical ceiling for the identical reason; it is the
price of a stat-only no-op and is accepted on those terms. The user-facing escape is
`unset __OCX_ENV_STATE` (C-012).

**Any test of this ceiling MUST force the collision** by explicitly setting the mtime back to the
recorded value — **never by writing quickly**. A test that races the clock is green on ext4 for the
wrong reason and red on a slow runner: the "green that cannot be told from never ran" class this
document refuses everywhere else.

**Walk cost is bounded, and the bound is verified rather than assumed:** `walk_for_project_file` probes
`.git` and `ocx.toml` at each level concurrently and returns `None` as soon as `.git` is present with no
hit at that level. Two details a cost claim must not elide: the boundary blocks *ascending past* a
level, not reading **at** it (an `ocx.toml` at the repo root still wins, since the candidate check runs
before the `.git` gate); and **any** `.git` entry counts — directory, git-worktree linkfile, or symlink
— with a non-`NotFound` I/O error failing closed to "boundary". `OCX_CEILING_PATH` bounds the walk the
same way, checked after the current level is probed.

The NFR benchmark must be measured **outside a repository with no ceiling set** — the unbounded case —
not the happy one.

**An indeterminate walk retains the scope; only a determinate one reverts.** `walk_for_project_file`
collapses a non-`NotFound` candidate error, a fail-closed `.git` boundary and a real miss into the same
`None`, so the reconciler cannot tell them apart from the return value. Before reverting a project
scope on a walk that produced **no hit or a different hit**, the reconciler MUST therefore run one
determinacy check: if `L.scopes.project.dir` is still an **ancestor-or-self of the CWD** *and*
`symlink_metadata(dir/ocx.toml)` reports a **regular file**, the walk's answer is **indeterminate** —
retain the scope unchanged and **emit nothing**. A genuine leave (CWD outside `dir`), a genuine
deletion (`NotFound`) and `OCX_NO_PROJECT=1` all fail that check and revert normally (C-007a, C-017).
The extra `stat` is on the **revert** path only, never on the no-op path — cheaper than plumbing a
tri-state through a loader every command shares, and it matches Decision 2's own "an indeterminate
probe retains the stamp" posture (C-023).

Three adjacent cases need **no new code**, only an assertion:
- an `ENAMETOOLONG`-class probe error — `has_git_dir` already fails closed to "boundary" on any
  non-`NotFound` I/O error (`crates/ocx_lib/src/config/loader.rs:704-716`), which stops the ascent at
  that level;
- termination at the filesystem root — `walk_for_project_file`'s `current.parent()` match already
  returns `None` (`config/loader.rs:688-692`);
- a `std::env::current_dir()` failure (the CWD itself unlinked) — degrade to "no project resolved this
  prompt", log at **debug**, **exit 0**, and **never** fall back to a cached CWD (C-051).

---

#### C-020 — Detection is file-state based, never command based
**ADR:** Decision 3 ("Detection is file-state based").

Nothing in this design intercepts, wraps or observes an ocx invocation in order to decide that a
recompose is due. The fingerprint watch set is the **only** trigger. So all of these are literally the
same event:

- a `git checkout` that swaps `ocx.lock`
- an `ocx.lock` copied in by a CI step or another process
- an editor writing `ocx.toml`
- `self update` moving the binary version
- **another shell's `ocx add --global`**

The wrapper function (C-047) is **not** an exception: it re-runs the same fingerprint check; it does not
report what the command it wrapped did.

Contracted as a structural guard: no reconcile-path code may read a marker file, an IPC channel, a
command log, or an env var written by an ocx subcommand in order to decide freshness.

---

#### C-021 — In-process / emitted parity, and entry iteration order
**ADR:** Decision 3, Component Contracts ("In-process / emitted parity" row).

The claim *"`move_to_front` / `export_path`, byte-identical between the in-process and emitted forms"*
is **required, not asserted**. `shell.rs` today ships per-shell *idempotency* tests against real shells
(`live_bash_zsh_idempotent_move_to_front`, `live_posix_…`, `live_fish_…`, `live_powershell_…`,
`live_batch_…`) and **none compares an emitted result against `utility::path::move_to_front`'s**.

The reconciler is the first consumer that applies in-process on one prompt and via emitted text on
another *in the same session*, so a divergence surfaces as **PATH order flapping between prompts**.

Two parity contracts, each with a test:
1. **Per arm**: emitted `export_path` result == `utility::path::move_to_front` result, byte for byte,
   over a shared fixture table.
2. **Entry iteration order**: the reconciler is a **new consumer** of the composer's `Vec<Entry>` and
   preserves `entrypoints/ > bin/ > shims/` only if it iterates in the same order the emit path does
   (`composer.rs:1072-1102` — consumers *prepend*, so the last entry pushed is first in PATH). Name
   this as a contract with a test, not as an outcome.

**Extended by [A-15](./adr_shell_env_addenda.md), [A-17](./adr_shell_env_addenda.md),
[A-18](./adr_shell_env_addenda.md), [A-19](./adr_shell_env_addenda.md)** — the parity contract needs a
**third clause**: `export_constant` must be byte-identical to `apply_entries`' `set` (`env.rs:594`),
because the `C == L.applied` exit guard compares exactly those two products, and measured they differ
today for any `!`-bearing value on all five POSIX arms (A-15). Two emit-side corrections are
prerequisites for parity: `export_path` gains the empty-value comment guard `export_list` already has
(`shell.rs:331-333`), since `Bash.export_path("PATH", "")` against ambient `/a:/b` yields `:/a:/b` — a
CWD-on-PATH primitive — while `move_to_front` refuses to prepend an empty value (A-17); and the
`Bash | Zsh` arm gains a second fixpoint loop collapsing `::` → `:` between the value-removal loop and
the leading/trailing strip, because six arms and the in-process path strip ambient empty segments and
bash/zsh are the outlier (A-18). Finally, the contract must name the **comparison predicate**: on
Windows the guard is ASCII-case-insensitive, so `C == L.applied` is **not `==`** (A-19).

---

### 1.3 Project state and consent

#### C-022 — `StateStore` project-scoped accessors
**ADR:** Decision 2 (Contract).

```rust
// crates/ocx_lib/src/file_structure/state_store.rs
impl StateStore {
    /// `{root}/projects/<key>/` — project-scoped state, keyed by
    /// `ReferenceManager::name_for_path(canonical_project_dir)`.
    pub fn project_state_dir(&self, key: &str) -> PathBuf;
    /// `{root}/projects/<key>/consent.json`
    pub fn consent_stamp_file(&self, key: &str) -> PathBuf;
    /// `{root}/projects/` — the sweep root for `ocx clean`.
    pub fn project_state_root(&self) -> PathBuf;
}
```

- **Key** is
  `ReferenceManager::name_for_path(dunce::canonicalize(canonicalize(<resolved project FILE>).parent()))`
  — first 16 hex of SHA-256, unchanged, already shared by `refs/symlinks/` and the projects ledger.
  **Canonicalize the file, take its parent, then `dunce`.** Both calls are load-bearing and their order
  fixes the Windows form: `tokio::fs::canonicalize` on the config file
  (`crates/ocx_lib/src/project/registry.rs:196-198`) and `dunce::canonicalize` on the directory
  (`register`, same file) do not produce the same string, and the ledger's key is the second form.
  This is the **shipped ledger derivation** — reuse `register_project_dir_best_effort`'s as **one
  shared helper**, never a second, directory-based derivation. That single canonical directory is the
  input to `name_for_path`, to the stamp's `project_dir` field (C-024) and to the `paths` compare
  (C-030).
- Canonicalizing the **file** is also the safer direction. `resolve_explicit_project_path` follows
  symlinks by design and returns the **un-canonicalized** path, so `OCX_PROJECT=/w/fake/ocx.toml`
  symlinked to `/attacker/ocx.toml` would otherwise activate the attacker's config under a `paths`
  grant on `/w/fake`; canonicalizing makes the identity `/attacker`, which is not granted ⇒ `Inert`.
  It also collapses aliased lookups (relative segments, symlinks) to one entry — `registry.rs:175-186`
  documents that purpose verbatim.
- Follows the shipped `StateStore` shape: typed accessor returning `PathBuf`, derivation documented
  inline, atomic-write mechanism named in the doc comment.
- **Writes** go through `write_bytes_atomic`; a stamp is **replaced, never edited in place**, so a
  future multi-writer surface here uses `lock_scoped` into `$OCX_HOME/locks`, never a sidecar.
- **Pre-`Context` reachability**: `self activate` already constructs `FileStructure::new()`, which owns
  `state`. No pure associated fn is needed (unlike `managed_config_snapshot_path`).

**Two directories named `projects`, with opposite lifetimes** — `$OCX_HOME/projects/` (the symlink
ledger: GC truth, lifetime tied to installs) and `$OCX_HOME/state/projects/` (consent stamps: deletable
at any time). Name the distinction in `subsystem-file-structure.md`, not only here (C-052).

**Not chosen, recorded so it is not re-proposed:** promoting `projects/<hash>` to a directory. That
entry is a **liveness oracle** hardened across two adversarial findings (SEC-1 three-state probe,
CODEX-BLOCK-1 TOCTOU re-probe, CODEX-BLOCK-2 atomic staging); `probe_live_target` returns `Dead` for a
non-symlink, so every pre-existing ledger entry would read dead on first run after upgrade → live GC
roots dropped → **pinned packages collected**.

---

#### C-023 — `ocx clean` sweeps `state/projects/`
**ADR:** Decision 2 (GC bullet, sweep-guards bullet).

`ocx clean` removes `state/projects/<key>/` **iff the stamp's own `project_dir` no longer exists on
disk.** Explicitly **not** `{ name_for_path(dir) | dir ∈ live_projects() }`:

1. That re-derives a key the ledger already stores *as the entry filename* from a re-canonicalized
   target, and the two derivations can diverge — a changed parent symlink or a case-normalizing
   filesystem yields a different 16-hex for the same live project, sweeping its state while its ledger
   link survives.
2. The ledger's population rule is strictly narrower than the consent-writer list: registration happens
   only on a lock **save** or a mutation **commit**, and `live_projects()` further requires
   `<target>/ocx.lock` to exist. So an **`[env]`-only project** — which C-015/C-019 make a first-class
   case — can never be in the ledger, and its consent stamp would be revoked by *every* `ocx clean`,
   silently, forever.

**Guards, restated because a new call site does not inherit them:**
- `symlink_metadata` on `<key>` — a symlinked state directory is **skipped, never followed** into a
  `remove_dir_all`.
- A **re-probe of `project_dir` immediately before removal** (the CODEX-BLOCK-1 TOCTOU pattern).
- A **skip for `.tmp-*` staging names**.
- An **indeterminate probe retains** the stamp. Over-retention leaves a stale stamp for a project that
  still exists; under-retention makes a live project inert.

**The sweep honours `dry_run` on the same terms as every other removal path.**
`PackageManager::clean` is `clean(&self, dry_run: bool, force: bool)`
(`crates/ocx_lib/src/package_manager/tasks/clean.rs:365`) and threads `dry_run` through every removal
path; `CleanResult.held_by` is populated only in dry-run. The consent-stamp sweep is no exception:
`ocx clean --dry-run` must never delete a consent stamp, and every swept stamp appears in `CleanResult`
so a real `ocx clean` never revokes consent silently. **Named red state:** make the sweep ignore
`dry_run` and the dry-run test goes red.

**No global-tier carve-out.** Dropped as unreachable-by-construction: the global scope needs no consent
stamp at all, so nothing ever writes that directory, the exemption never fires, and no test can tell a
correct implementation from a missing one. If global-tier state ever lands under `state/projects/`, the
carve-out returns *with* the test that reds without it.

This **amends** `subsystem-file-structure.md`'s "`state/` is not walked by `ocx clean`" bullet — that
edit is mandatory, not optional (C-052).

**Extended by [A-31](./adr_shell_env_addenda.md)** — removal requires **both** preconditions: the stamp
deserializes at a `v` this binary understands, **and** the pre-removal re-probe proves `project_dir`
definitively absent. Unreadable, malformed, unknown-`v` and indeterminate-probe all **retain**, with
one line at debug. An unreadable stamp is already inert at `evaluate` (C-025), so sweeping it buys
nothing, while under-retention deletes consent a newer or rolled-back binary wrote.

---

#### C-024 — `ConsentStamp` and `record`
**ADR:** Decision 4 (Consent stamp, Write seam).

```rust
// crates/ocx_lib/src/project/consent.rs (new)
pub struct ConsentStamp {
    pub v: u8,
    pub project_dir: PathBuf,
    pub sources: BTreeSet<String>,
    pub stamped_at: OffsetDateTime,
}
pub fn record(project_dir: &Path, sources: &BTreeSet<String>) -> crate::Result<()>;
```

- Written to `state/projects/<key>/consent.json` via `write_bytes_atomic` (C-022).
- **Write seam**: any explicit project-scoped ocx command — `add`, `remove`, `lock`, `update`, `pull`,
  `run`. That is **not** the ledger's registration seam: `register_project_dir_best_effort` has exactly
  two call sites (lock save, mutation commit) and **`run` and `pull` reach neither**. The stamp needs
  its own seam, high enough in the project-command path to cover all six. A plan must schedule it as
  such rather than assuming it rides along.

**Extended by [A-25](./adr_shell_env_addenda.md), [A-29](./adr_shell_env_addenda.md)** — the reader is
`consent::load(key) -> Option<ConsentStamp>`, returning `None` on **every** failure (I/O error, JSON
parse error, unknown field, or a `v` this binary does not recognise), logged at debug and never warned;
`ConsentStamp` carries `#[serde(deny_unknown_fields)]` with **all four fields required** — no
`#[serde(default)]` on `sources` or `project_dir` — so a truncated stamp can never deserialize into a
valid-looking one (A-25). The write seam is stated as a **negative contract**: `record()` is
`pub(crate)`, its doc comment names the allowlist, and the six commands are **exactly** the writers —
every other command, explicitly including `ocx env`, `ocx inspect`, `ocx shell state`,
`ocx self activate` (with and without `--reconcile`), `ocx list`, `ocx direnv export` and
`ocx completions`, MUST NOT create or modify `state/projects/<key>/`. Enforcement is the acceptance
test, not discipline (A-29).

---

#### C-025 — `evaluate` — the activation predicate
**ADR:** Decision 4 (whole decision).

```rust
pub enum Decision { Activate(Grant), Inert(Reason) }
pub enum Grant { Stamp, Namespace, Path }   // which clause granted — the grants differ in strength
pub fn evaluate(
    project_dir: &Path,          // canonical
    lock_sources: Option<&BTreeSet<String>>,   // the lock's CLAIM; None = absent/unreadable/unparseable
    verified: Option<&BTreeSet<String>>,       // the store's RECORD for that same parsed lock;
                                               // None = corroboration failed, never "absent lock"
    whitelist: &ShellConsent,
) -> Decision;
```

**Activation is permitted iff any of:**
1. a valid consent stamp exists for this project **and** the current lock's **claimed** source set ⊆
   the stamped source set ⇒ `Activate(Grant::Stamp)`; **or**
2. the **store-corroborated** source set is present, **non-empty**, and every source in it matches the
   namespace whitelist ⇒ `Activate(Grant::Namespace)`; **or**
3. the project's canonical directory is in the path whitelist ⇒ `Activate(Grant::Path)`.

Otherwise: **zero env change, one hint line.** A fresh clone is inert.

**Clause 2 never reads the lock's claim — A-39, normative and in terms.** The package store is keyed by
`(registry, digest)` alone, so `ocx.lock`'s `repository` field never has to be true for the content to
be found: a lock can pair a granted org's name with a digest that came from any repository on that
registry. `verified` is `verified_sources` — the store's own `refs/origins/` record of which logical repository
this host resolved and materialized each locked digest under — and clause 2 quantifies over that and
nothing else. Read the write gate literally: `record_origin` fires from the fetching branch of
`setup_owned_impl` under `from_registry = provided_metadata.is_none()`, which excludes the local-tarball
path and nothing else, so a marker attests *an act of pulling on this host under that name*, not that a
registry served under it (A-39's residual — [ocx-sh/ocx#348](https://github.com/ocx-sh/ocx/issues/348)). A tool the store cannot corroborate poisons the whole grant, and
the resulting `None` is a **refusal**, never "unconstrained". Where the claim would have granted and the
record does not corroborate it, the refusal is `Reason::UncorroboratedNamespace`, carrying both sets.
**Clause 1 keeps the claim deliberately**: a stamp records what the lock said at the time, and its drift
detection is a comparison against that same claim.

**Evaluation order is load-bearing, not cosmetic.** Clause 3 runs first and before the lock is consulted
at all — a `paths` grant is the one clause that holds for a project whose lock cannot be read. Then a
`None` `lock_sources` ends in `Inert(LockUnavailable)`. Then clause 1 before clause 2, because the two
grant *different amounts* (below) and when both hold the answer must be the stronger one.

**Non-vacuity is normative.** Without the non-empty requirement in clause 2, an *empty* source set
satisfies "every source matches" for any user, with no stamp and no whitelist entry — and the project
that produces an empty source set is precisely the one this decision exists to stop. Not hypothetical: a
relative `path` value in project `[env]` resolves against the project root, so a clone carrying
`[env] PATH = { type = "path", value = "bin" }` and **no `ocx.lock` at all** would activate through
clause 2 and put `<clone>/bin` PATH-front on `cd`. Clause 1 is unaffected — a stamp with an empty
`sources` set is still consent; the emptiness that must never grant activation is the *unstamped* kind.

**A missing `ocx.lock` is treated exactly as an unreadable one**: no activation, one hint line. Absent,
unreadable and unparseable share one outcome, because all three leave the source-set predicate with
nothing to quantify over. Clause 3 is the deliberate exception.

**Project `[env]` is gated by clause 1 or clause 3, never by clause 2.** "`[env]` applies on its own
authority independently of the lock" (C-019) is about *composition and the watch set*, not
*activation*. Two conditions, not one: nothing sourced from `ocx.toml` is applied unless this predicate
returns `Activate(_)`, **and** the project-file `[env]` channel additionally requires the granting
`Grant` to satisfy `Grant::authorizes_project_env` — true for `Stamp` and `Path`, **false for
`Namespace`**. So `Activate(Grant::Namespace)` composes the project's **tools** and withholds its
`[env]`; the earlier flat reading ("nothing … unless this returns `Activate`") is false for exactly that
case. Why: clause 2's evidence vouches for a *published package* this host pulled under the
granted namespace — on a cold store that needs a credential to publish into it, and on a warm one it
needs at least a local pull naming it (A-39's residual, [ocx-sh/ocx#348](https://github.com/ocx-sh/ocx/issues/348)).
`[env]` clears neither bar: no publisher, no pull, not even a lock entry — a relative `type = "path"`
value resolves against the project root, so one line of `ocx.toml` puts `<clone>/bin` in front of `PATH`.
The gate therefore stands on the weaker floor unchanged.

**`evaluate` compares `project_dir`, not just the key.** `name_for_path` is SHA-256 truncated to 8 bytes
— a 64-bit name. Second preimage is out of reach, so this is not a live attack, but the stamp already
carries `project_dir` and reading it costs nothing: **`evaluate` rejects a stamp whose `project_dir` is
not the canonical directory under evaluation.** The key is a lookup index; the path is the identity.

**The two grants are independent and OR'd.** Clause 3 (a `paths` hit) and clause 2 (a `namespaces` hit)
each activate on their own, and **neither constrains the other**. Requiring both would break both cases
by construction. **An absent or empty grant grants nothing; it never means "everything allowed".** A
strict AND mode is a coherent product and is **not shipped** — a separate decision, not an open question.

`Reason` is the enumerated inertness set `ocx shell state` renders (C-052):
`NoStampNoGrant { derived_sources, paths_tested, namespaces_tested }`,
`SourceSetDrift { new_sources }`,
`UncorroboratedNamespace { claimed_sources, verified_sources }` (A-39),
`HookDisabled { rung, tier }`,
`YieldedTo { tool, signal }`, `LedgerOverCap { scope }`, `LedgerUnreadable { first_prompt: bool }`,
`LockUnavailable`.

`UncorroboratedNamespace` **outranks** the stamp-drift refusal, because it names the clause that was
about to grant: a drifted stamp may also be true, but it is not why this project stayed inert.

`ASSUMPTION:` `HookDisabled` and `YieldedTo` are `Reason` variants even though they are decided outside
`evaluate` proper. Justification: Decision 10 enumerates them in one list as "the reason the shell is
not active", and a single enum is what makes `ocx shell state`'s output total; the deciding call site
constructs the variant, `evaluate` never returns those two.

**The superseded "first-consented tool-name set" identity guard is dropped**: a `git pull` adding a tool
from an already-consented namespace would re-prompt, which is the ceremony fatigue that trains blind
confirmation. Path reuse is covered by the source-set predicate for the case that matters and accepted
otherwise, as direnv, mise and Nix all accept it.

**Extended by [A-25](./adr_shell_env_addenda.md), [A-26](./adr_shell_env_addenda.md),
[A-39](./adr_shell_env_addenda.md)** — an **unusable
stamp is an absent stamp**: on any `load()` failure clause 1 simply fails while clauses 2 and 3 still
evaluate, logged at debug, never warned, never an error (A-25). And **clause 3 grants activation
directly and unconditionally, every prompt, writing no stamp** — a `paths` grant is deliberately
drift-blind and its revocation therefore immediately effective, while clause 2 stays drift-sensitive by
its own quantifier because it re-derives the store-corroborated source set and re-evaluates it every
prompt (A-26, A-39, C-027).

---

#### C-026 — Source normalization: `<registry>/<first-path-segment>`, off the LOGICAL coordinate
**ADR:** Decision 4 ("A `source` is `<registry>/<first-path-segment>`, normative"; "The predicate is
defined over the logical coordinate").

Derivation, for each `LockedTool.repository: Identifier`:
1. Take the **logical** repository coordinate the lock records. **Never** a re-derived physical address.
2. **Lowercase** the registry host.
3. **Preserve the port** — `localhost:5000` is a distinct source from `localhost`.
4. **Spell the default registry explicitly** (`ocx.sh/...`), never elided.
5. **Truncate the repository path to its first segment.**

`ghcr.io/acme/tools/cmake:3.28` ⇒ `ghcr.io/acme`.

Registry granularity alone would be nearly vacuous (consent to one GHCR org consents to all of GHCR);
full-repository granularity would re-prompt on every ordinary tool addition. The first path segment is
the org — the unit an operator controls and the unit an attacker must register.

**This is the same string the `namespaces` whitelist matches against** — one normalization, now **three**
surfaces, because A-39 added the third. The store's `refs/origins/` markers hold a full
`<registry>/<repository-path>` coordinate, and `source_of_origin` truncates it by routing **back through
the same `source_of`** rather than re-implementing the rule — so the lock's claim and the store's record
are normalized by exactly one function and can be compared at all. A malformed marker yields `None`,
which fails the whole grant closed. Which of the two sets a clause may *grant* on is C-025's question,
not this one: clause 2 takes the record, clause 1 takes the claim, and both are this same string.

**Correction, carried from discovery — this needs new code.** `Identifier`
(`crates/ocx_lib/src/oci/identifier.rs`) has `registry()` :169, `repository()` :174, `name()` :182,
`tag()` :191, `digest()` :201 — and **no first-path-segment accessor**. A new accessor (or a free
function in `project::consent`) must be written; nothing existing returns this.

`ASSUMPTION:` the accessor lands on `Identifier` rather than in `project::consent`. Justification: it
is a property of the coordinate, not of consent, and a second consumer (diagnostics in
`ocx shell state`) already exists in this design — the repo rule is extend existing mechanisms.

**Residual, accepted and documented** (same seam as C-034/Decision 8): a change in the physical registry
an index routes an already-consented logical namespace to **never triggers re-confirmation**. For an
entry already in the lock the content is digest-pinned, so an index redirect cannot silently swap bytes
under a standing stamp; the exposure is at `ocx lock` / `ocx update` time. The instrument for that is an
**operator-tier `[[trust.policy]]` plus signature verification**, not a re-shaped consent predicate —
re-shaping consent around the physical address would pin routing, the failure
`adr_lock_records_physical_address.md` was **Rejected** for.

**Test that must red on a fault injection:** build the source set from a re-derived physical address and
watch the consent test fail.

---

#### C-027 — Grants do not stamp; nothing on the activation path writes a stamp
**ADR:** Decision 4 ("A grant is permission to stamp without asking"), **deleted outright by**
[A-26](./adr_shell_env_addenda.md).

**`paths` and `namespaces` grant activation directly. They do not stamp.** No activation — first or
later — writes `state/projects/<key>/`. Clause 3 (C-025) activates on its own authority, **every
prompt, regardless of the lock's source set**; a grant is never converted into a stamped baseline.

The two grants are not symmetric, and that is the whole answer:

- **`namespaces` is drift-sensitive by its own quantifier.** Clause 2 re-derives and re-quantifies over
  the *current* **store-corroborated** source set at every prompt (A-39), so drift detection for it is
  **structural** and needs no stamp: a source leaving the grant — or one the store stops corroborating —
  makes the project inert at the next prompt. The premise that the source-set
  predicate would be dead code without an auto-stamp is false for clause 2.
- **`paths` is a directory grant and deliberately drift-blind.** Its stated use case — the devcontainer
  publisher — is precisely one where the operator *cannot* enumerate sources; making it
  drift-sensitive breaks it on the first `git pull` that adds a tool, in an environment with no human
  at the prompt to re-confirm. These are git `safe.directory` semantics: exact directory,
  unconditional, no content re-check.

Consequences, each testable:
- **Revoking a grant is immediately effective**, because no stamp was ever derived from it.
- The stamp write seam stays at **exactly six commands** (C-024) with no seventh writer, and no write
  lands on the stat-only per-prompt path (C-042).
- `ocx shell state` reports *"active via `paths` grant; source-set drift is not tracked for path
  grants"* — truthful rather than phantom (C-050).

**Named red state, both arms required:** re-introduce an auto-stamp and the assertion that
`state/projects/<key>/` stays absent fails; make clause 3 conditional on clause 1 and the `Activate`
assertion fails.

Grant roles, owner-confirmed and unchanged: **`paths` is the primary grant** (devcontainer/checkout
case); **`namespaces` is optional, an auto-enabler for projects *outside* the path grants** (fleet
case); the **global toolchain is always trusted — consent is project-scope only**.

---

#### C-028 — Consent before *parse*, not merely before *apply*
**ADR:** Decision 4 (Sequencing — the mise CVE lesson).

**Normative ordering rule:** the only project-supplied bytes read before consent is established are
(i) the CWD walk's `stat` calls and (ii) the `ocx.lock` parse the source-set predicate requires.
**`ProjectConfig` deserialization happens after.**

"Zero env change" is satisfiable by compose-then-discard, which would already have deserialized the
untrusted `ocx.toml`. The lock parse is an explicit, bounded carve-out, not an aside. A lock that is
unreadable or unparseable means **no activation** plus one hint line.

The residual is small either way — project `[env]` is literal-only with no interpolation — but the mise
CVE is a lesson about ordering, and ordering is cheap to state once. This is structurally safe here in a
way it was not for mise, **because the whitelist can only come from `config.toml`** (C-035).

**Extended by [A-33](./adr_shell_env_addenda.md)** — "from `config.toml`" includes the **explicit**
tier: a file named by `--config` / `OCX_CONFIG` may carry `[shell.consent]` and is a third
consent-bearing channel, of the same already-out-of-scope threat class as `OCX_CONSENT_*` (a hostile
parent process). `OCX_NO_CONFIG=1` does **not** prune it — only `OCX_NO_HOOK=1` makes a shell wholly
inert.

---

### 1.4 Configuration surface

#### C-029 — `ShellConfig` / `ShellConsent`, and the `deny_unknown_fields` split
**ADR:** Decision 4 (Whitelist location and grammar), Decision 5, Component Contracts
(`ocx_lib::config::shell` row).

```rust
// crates/ocx_lib/src/config/shell.rs (new)
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ShellConfig {              // NO deny_unknown_fields — fleet forward-compat
    pub hook: Option<bool>,
    pub completions: Option<bool>,
    pub consent: Option<ShellConsent>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]         // consent-bearing table — the one place tolerance stops
pub struct ShellConsent {
    pub paths: Vec<PathBuf>,
    pub namespaces: Option<ConsentScopeSpec>,   // ONE ConsentScopeSpec, never a Vec
}
```

Added to `Config` as `pub shell: Option<ShellConfig>`.

**The `deny_unknown_fields` split is the contract, not a detail.** `ShellConfig`/`hook`/`completions`
omit it, like every other `Config` sub-struct, for fleet forward-compat. `ShellConsent` **carries** it,
verbatim for the reason `trust.rs:252-257` already records: *"A table carrying only keys a newer ocx
understands is therefore refused, not read as an accidental catch-all — the one place the fleet
forward-compat tolerance stops, because here dropping the unknown key would widen trust rather than
narrow it."* An operator publishing `namespaces` plus a future *narrowing* key an older fleet host does
not know must have that host **refuse** the payload, not silently drop the key and activate on the full
namespace.

Test pair: `[shell] futurekey = 1` loads fine; `[shell.consent] futurekey = 1` fails the file.

**`namespaces` deserializes through a strict wrapper, `ConsentScopeSpec` — not `ScopeSpec` verbatim.**
Verified defect: `trust.rs`'s hand-written `ScopeSpec` deserializer (`visit_map`) drops unknown keys
inside the table with `_ => { map.next_value::<serde::de::IgnoredAny>()?; }`, commented *"Fleet
forward-compat, same as one level up: a key a newer ocx added is dropped, never a hard failure."* The
refusal fires only when both `include` and `exclude` are absent. Reusing `ScopeSpec` verbatim for
`[shell.consent] namespaces` means a future **narrowing** key inside the `namespaces` table would be
silently dropped on an older host — which **widens** consent, the exact outcome this contract says must
not happen. `ConsentScopeSpec` reuses `ScopeSpec`'s matching semantics and its string/object grammar
(C-030) but **refuses** an unknown key inside the table instead of ignoring it. `[[trust.policy]]`'s own
`ScopeSpec` **keeps its tolerant behaviour unchanged** — this is an additional strict wrapper, not a
change to the shipped type.

**Named red state:** a `namespaces` table carrying `include` plus one unknown key must fail to
deserialize; delete the strict wrapper and the test goes green, which is the failure direction.

**Extended by [A-27](./adr_shell_env_addenda.md), [A-33](./adr_shell_env_addenda.md)** — the strict
wrapper is concretely a `#[serde(deserialize_with = …)]` on `ShellConsent::namespaces` pointing at a
**consent-scoped visitor** that mirrors `ScopeSpec`'s hand-written one but replaces its unknown-key arm
with an error, keeps the neither-key floor, and runs the pattern validator (C-030) on **every** pattern
before constructing `ScopeSpec::Set`. `deny_unknown_fields` on `ShellConsent` alone does **not** deliver
the stated property — the tolerance sits one level deeper, in `visit_map` (`trust.rs:342-346`) — and the
shared `ScopeSpec` deserializer is **not** changed, its tolerance being deliberate for
`[[trust.policy]]` (A-27). An explicit-tier file (`--config` / `OCX_CONFIG`) may carry
`[shell.consent]`, and C-034's managed digest gate does **not** apply to it (A-33).

---

#### C-030 — `namespaces` grammar, enforced at parse
**ADR:** Decision 4 (the `ScopeSpec` bullet and the grammar bullet).

`namespaces` is **one `ConsentScopeSpec`**, a consent-specific wrapper around
`ocx_lib::trust::ScopeSpec` (`trust.rs:247`) — not `Vec<ScopeSpec>`, not `Vec<String>`, and not
`ScopeSpec` verbatim (see C-029: the shipped `ScopeSpec` deserializer tolerates an unknown key inside the
table, and that tolerance is unsafe on this surface). A flat list can only ever widen, so "everything
under `ocx.sh/acme/*` except the one compromised namespace" is unspellable. A single spec is *either* a
fixed string *or* `Set { include, exclude }`, which states coverage and carve-out in one value; the
string form, the object form, the segment-bounded match, `specificity_for` (`trust.rs:419`) and the
hand-written `JsonSchema` all come for free from `ScopeSpec` — `ConsentScopeSpec` reuses them and adds
only the strict-refusal deserializer.

**Sharing the type couples no policy.** `ScopeSpec` is a matching primitive, not a trust semantic. A
namespace consented for shell activation is **not** thereby trusted for signature verification, and a
namespace named in `[[trust.policy]]` grants **no** activation consent. Two code paths, two questions,
neither reads the other's configuration.

**Parse-time rules — for `[shell.consent] namespaces` only:**
1. **Reject** any pattern containing `*` anywhere other than as a **final `/*`**. Reject a bare `*`.
   Reject the empty pattern. Reject an **empty `include`** (`ScopeSpec`'s general catch-all spelling is
   deliberately **not** inherited here).
2. **Match** by stripping the trailing `/*` and taking `pattern_matches`' no-wildcard branch —
   `target == pattern || target.starts_with(&format!("{pattern}/"))`.
3. `*` is optional-but-accepted for the org form. Descendant implication is **vacuous at source
   granularity**: a source is exactly two components, so `ocx.sh/acme/*` and `ocx.sh/acme` match the
   identical set. A third component (`ocx.sh/acme/team`) is a repository, not a source, and is
   **rejected at parse** (A-27). There is no whole-registry form — see the amendment below.

> **Amended 2026-08-25 — the whole-registry spelling is withdrawn**
> ([#344](https://github.com/ocx-sh/ocx/issues/344)). `<host>/*` and a bare `<host>` are both refused,
> as one error class, so neither is the way to spell what the other no longer says. The reason is the
> only bound clause 2 has: an attacker must get content onto this host under the granted namespace,
> which for content the host does not already hold needs that namespace's publish credential (for content
> it does hold, less — A-39's residual). A pattern spanning every organization on a host voids that bound
> wherever anyone can register on the host, and ocx cannot tell an open registry from a closed one — so
> the spelling is refused rather than documented as dangerous. An operator who genuinely trusts a whole
> private registry lists its organizations, or uses a `paths` grant. Consequence, stated: `exclude` no
> longer subtracts from a wide grant; it subtracts an organization one tier included from another tier's
> view, which is what `accumulate`'s exclusion-wins rule is for.

Why (1) is mandatory: `trust::pattern_matches` (`trust.rs:374-383`) is segment-bounded **only on its
no-wildcard branch**, and its own doc comment calls a bare `ghcr.io/acme*` *"an intentional substring
glob"*. So `ocx.sh/acme-corp*`, one keystroke from the intended spelling, would match
`ocx.sh/acme-corp-evil/tool`. `[[trust.policy]]` keeps its looser grammar; it is signature-gated and
this surface is not.

**The "no typosquat at the OCI level" rationale is withdrawn as false.** On a public registry the vector
is *relocated* to a primitive cheaper than creating a sibling directory — registering the org
`acme-corp-evil` on `ghcr.io`, `docker.io` or the default `ocx.sh`.

**Malformed ⇒ refuse, never partially parse**, per channel:
- **`config.toml` tier**: an ordinary parse error on `[shell.consent]` ⇒ **that table is stripped and the
  rest of the file still applies**, on every tier, exit 0 with the reason recorded — *unless* the table
  carries a non-empty `namespaces.exclude`, which withdraws another tier's grant and therefore keeps the
  hard failure (78 on home / system / `--config`), because dropping a withdrawal widens (A-40). Either
  way the grant contributes nothing and nothing is partially parsed. The mechanism is **new machinery,
  not shipped behaviour**: `visit_str`
  (`trust.rs:325-329`) accepts *every* string, `""` included, and the `trust.rs:349-354` error covers
  only a table naming neither key. Nothing shipped validates a consent pattern today; the rejection
  comes from `validate_consent_pattern` run inside the consent-scoped deserializer (C-029).
- **Env channel**: see C-031 — the whole contribution is discarded with one warning; a hard error would
  break every prompt (D3).

There is **no whole-list-refusal contract** and no filtering-deserializer prohibition: with a single
spec there are no surviving entries, so the question does not arise.

**`paths` is exact-directory, matched on the canonicalized path.** No prefix, no glob (git ships
exact-only and documents that it has no glob; every prefix grammar shipped carries the sibling-typosquat
footgun). **Only the project side is canonicalized; whitelist entries are compared literally** (after
separator and trailing-slash normalization). Canonicalizing *entries* at read time would make the grant
follow a symlink an attacker may control on the parent (`/workspaces/repo → /tmp/evil` needs only write
access on `/workspaces`); comparing literally never matches a symlinked checkout and is then silently
**inert** — the fail-safe direction, and an operator writing an exact path can write the real one.

**No content-hash drift re-checking on the whitelist itself.** Static list, field default. Drift belongs
to the consent stamp (C-025), not the grammar.

**Extended by [A-27](./adr_shell_env_addenda.md), [A-28](./adr_shell_env_addenda.md)** — the grammar is
**one `fn validate_consent_pattern(&str) -> Result<()>`**, applied at deserialization to the bare
string form and to every element of `include` and `exclude`. It accepts **exactly two spellings**:
`<host>[:<port>]/<org>` and `<host>[:<port>]/<org>/*` (equivalent at source granularity). It rejects
eight classes — the whole-registry class (`<host>/*` and a bare `<host>`, per the 2026-08-25 amendment
above), plus: the empty string and a bare `*`; any `*` other than as
the final two bytes `/*`, and any pattern with more than one `*`; a trailing `/` with no `*`, and the
pattern `/*`; any empty `/`-delimited component; **any ASCII uppercase byte anywhere** (an uppercase
repository is refused outright by `Identifier` parsing, `oci/identifier.rs:505`, so such a pattern is
simply unmatchable); three or more components after stripping an optional trailing `/*`; and `@`
anywhere or `:` after the first `/`. Implementation: strip an optional trailing `/*`, require one or
two components, and validate the second through the **shipped `Identifier` repository validator**
rather than minting a second charset. A source is exactly two components (C-026), so descendant
implication is **vacuous at source granularity** — `ocx.sh/acme/*` and `ocx.sh/acme` match the same
set, and a three-component pattern is rejected (A-27). `paths` entries stay a **literal byte compare**
after separator and trailing-slash normalization — no case folding, no canonicalization of the entry,
a case-only mismatch is `Inert` — with a **near-miss** line in `ocx shell state`'s reason enumeration
when an entry differs from the canonical directory only by ASCII case or separator style (A-28).

---

#### C-031 — The env channel: `OCX_CONSENT_PATHS` / `OCX_CONSENT_NAMESPACES`
**ADR:** Decision 4 (Env channel; Empty tokens bullet).

| Var | Separator | Folds into |
|---|---|---|
| `OCX_CONSENT_PATHS` | **OS PATH separator** (mirrors `MISE_TRUSTED_CONFIG_PATHS`) | accumulated `paths` |
| `OCX_CONSENT_NAMESPACES` | **comma** — a registry may carry a port (`localhost:5000/acme/*`), so `:` is unusable on Unix | accumulated `include` set |

Both are **additive**: unioned with the config tiers, never a replacement, never higher-precedence. A
hostile parent process setting them is out of scope, consistent with every surveyed tool.

**Empty tokens are discarded, never converted to a pattern — normative, both vars.**
`trust::pattern_matches` returns `true` for an empty pattern and its own doc comment states *"An empty
pattern is a catch-all"* (`trust.rs:373-376`). Without this rule a trailing comma, a doubled comma or the
empty string would contribute one empty pattern consenting to **every** namespace:
`OCX_CONSENT_NAMESPACES='ocx.sh/acme/*,'` would be indistinguishable from `'*'`, through a channel a
devcontainer image writes by hand.

Contract: split on the separator, trim each token, **drop every empty token before any `ScopeSpec` is
constructed**. An all-empty value (`''`, `','`, `',,'`) contributes **nothing** — never a catch-all, and
**never an error**, because an unset var and an empty one are the same situation and D3 forbids breaking
a prompt over either. `OCX_CONSENT_PATHS` follows the same rule: an empty token must never become an
empty `PathBuf`, which normalizes toward a root rather than toward nothing.

**Whole-contribution refusal on a malformed (non-empty) pattern**: discard the **entire**
`OCX_CONSENT_NAMESPACES` contribution with **one warning**; the config tiers stand alone. Neither channel
activates on a partially-parsed spec, and neither widens.

**Discriminating test (the ADR names the red state; ship it):** feed `'ocx.sh/acme/*,'`, `'a,,b'`, `','`
and `''`; assert each yields exactly its non-empty patterns and grants nothing else, asserted by an
untrusted source (`ghcr.io/evil/tool`) **not** matching. Produce the red by feeding the same values
through a parser that keeps empty tokens — that variant **must** make the untrusted source match, which
is what proves the assertion discriminates rather than passing vacuously. Same pair for
`OCX_CONSENT_PATHS`.

**Extended by [A-27](./adr_shell_env_addenda.md), [A-40](./adr_shell_env_addenda.md)** — "malformed" is
decided by the **same** `validate_consent_pattern` the config tiers use (C-030); one validator, two
channels. The per-channel split is only in the consequence, and A-40 corrects the config side: a
rejected pattern makes `[shell.consent]` fail to deserialize, and the refused table is then **stripped**
— that grant contributes nothing while every other section of the file still applies, on **every** tier,
exit 0 with the reason recorded — *unless* the table carries a non-empty `namespaces.exclude`, the one
key that **withdraws** another tier's grant, where dropping it would widen and the file therefore keeps
the hard failure. On `OCX_CONSENT_NAMESPACES` the **whole** contribution is discarded with one warning
and the config tiers stand alone. Empty tokens are dropped before any pattern is constructed, unchanged.

---

#### C-032 — `Config::merge` semantics per new field
**ADR:** Decision 4 (Precedence bullet), Decision 7, Component Contracts.

| Field | Merge semantics |
|---|---|
| `shell.hook` | **scalar — higher tier wins if `Some`**, in **both** directions (C-034) |
| `shell.completions` | scalar, identical to `hook` |
| `shell.consent.paths` | **appends** |
| `shell.consent.namespaces` | **accumulates into one spec** — `include` ∪ `include`, `exclude` ∪ `exclude`; a string form contributes a single `include`; **no tier overrides another** |

**Two-level rule, no tier ordering:**
1. **Across tiers** the per-tier specs accumulate into one, following git.
2. **Within the accumulated spec**, a source is consented iff it matches at least one `include` **and no
   `exclude`** — carve-outs beat coverage regardless of which tier contributed either. An **empty
   `include` is rejected at parse** rather than read as catch-all (C-030).

`specificity_for` ranks *which* pattern explains a decision, for diagnostics only; it never resolves a
conflict, because exclusion already wins unconditionally.

**Fail-safe direction:** the only thing a lower tier can do to a higher tier's grant is **remove** it.

**Two justification corrections to carry:**
- *"There is no untrusted tier in the union"* is **withdrawn** — `trust.rs:36-38` names *"the untrusted
  managed-config payload"* in as many words, and C-034 gating that tier concedes the point. The union is
  safe because exclusion wins and no tier can override another, **not** because every contributor is
  trusted.
- `[[trust.policy]]` is a **storage-append analogue only**: `Config::merge` appends, then `trust::resolve`
  masks by specificity and `apply_system_locks` exempts the system tier. Say "plain union — *unlike*
  `[[trust.policy]]`, which masks by specificity". Citing it as an existing pattern oversells it.

**Producing obligation, verified gap.** `Config::merge` (`crates/ocx_lib/src/config.rs:145-201`) keeps no
tier provenance today, yet C-040 requires the CLI to report "which tier will win" and C-050 requires
`ocx shell state` to name "the tier that set it". `Config::merge` therefore gains: it records the
contributing tier for the scalar `shell.hook` and `shell.completions` fields — a `#[serde(skip)]` runtime
provenance field set by the loader, following the shipped `RegistryDefaults::system_locked` precedent at
`config.rs:134-136`.

**Extended by [A-32](./adr_shell_env_addenda.md)** — the tier order the provenance must record is
`fold_managed_tier` **then** `merged.merge(overlay.clone())`
(`crates/ocx_lib/src/config/loader.rs:180-182`): the managed tier beats the **discovered** chain
(system → user → home), and the explicit tiers `--config` / `OCX_CONFIG` beat the managed tier. A work
package that moves `merged.merge(overlay)` above the managed fold inverts this; that is the named red
state. The recorded provenance names whichever tier **actually** decided, never a hard-coded "managed".

---

#### C-033 — Project-tier `[shell]` strip, with tests that can go red
**ADR:** Decision 4 ("Never `ocx.toml` — and this needs a guard, not a citation").

Today a `[shell]` block in `ocx.toml` is a hard parse error because `ProjectConfig` and
`RawProjectConfig` both carry `#[serde(deny_unknown_fields)]`. But that attribute's own docstring states
its purpose as *"schema drift in consumer `ocx.toml` files surfaces as a parse error rather than silent
ignore"* — it is a **typo detector**, load-bearing for a security property in a different file, with
nothing recording the coupling and no test pinning it. Three sibling config structs already omit it
citing fleet forward-compat, and C-029 mandates exactly that doctrine for `Config`.

Worse: `ConfigLoader::load_with_local_view` **already resolves a project tier into the pipeline that
carries `Config`** and parks it (`let _project_path = Self::project_path(…)`, commented *"consumed in
later phases once the project-config schema lands"*). The untrusted tier is pre-wired one commit from
being read.

**Contract — convert the prose into a guard that can go red.** When `[shell]` lands on `Config`:
- **Strip `shell` from any project-tier contribution explicitly**, in `guard_managed_sigstore_trust`'s
  home and idiom (`crates/ocx_lib/src/config/loader.rs`).
- Ship **two** tests: (a) a project-tier fold **cannot** contribute `[shell.consent]`; (b) `[shell]` in
  `ocx.toml` is **refused**.

"Structurally, not by discipline" is only true once those exist; until then it *is* discipline.

---

#### C-034 — Managed-tier digest-pin gate on `[shell.consent]`; `[shell] hook` merges unconditionally
**ADR:** Decision 7 (OD-2).

**`[shell] hook` (and `completions`) merge unconditionally, in both directions, and that is
load-bearing.** `Config::merge` is scalar-wins-if-present with the managed tier folded after the
discovered system→user→home chain, so the managed tier **beats every discovered tier (system → user →
home)** — it can force `hook = true` over a user's explicit `hook = false`, not only the fleet-**off**
direction the "hooks off on build agents" rationale covers.

**The explicit tiers `--config` / `OCX_CONFIG` still merge on top of the managed fold, and win.**
Verified at `crates/ocx_lib/src/config/loader.rs:180-182` — `fold_managed_tier` runs, then
`merged.merge(overlay.clone())` runs after it — where the loader's own comment states the intent
verbatim: *"the managed tier folds in AFTER the discovered chain … but BELOW `OCX_CONFIG` and
`--config`, so the explicit tiers must merge on top of the managed fold — never underneath it."* That
ordering stands. `ocx shell state` names **the tier that actually decided the rung**, never asserts
"managed" (C-050).

What makes that acceptable is exactly one argument, stated as the load-bearing claim it is: **the toggle
grants nothing, because consent (C-025) still gates every project independently.** An operator who
already controls `[mirrors]`, `[patches]` and the default registry gains no escalation from a
prompt-hook toggle. **If that consent coupling ever weakens, this key stops being safe to merge
unconditionally.** `completions` follows a fortiori — it grants nothing and gates nothing.

**`[shell.consent]` merges only when the `[managed] source` is digest-pinned.** Otherwise it is
**stripped with a WARN naming the reason**. Reuse `guard_managed_sigstore_trust`'s home and warning
idiom (`config/loader.rs`) — it already honours `[trust.sigstore]`'s `trusted_root_json`, `fulcio_url`
and `rekor_url` only behind a digest-pinned source, because otherwise the consent material arrives over
the very channel it exists to verify and whoever can move the tag can swap it.

**Two things that guard needs and did not have:**
1. **A reader.** `log::warn!` goes to stderr, and C-044 puts this config read inside `ocx self activate`,
   whose stderr the shims discard. Route the reason through the same `printf … >&2`-inside-the-eval'd-
   script channel the rest of the hook uses, **and** record it where `ocx about` can surface it.
2. **A red state.** This is the *only* thing between an unpinned managed payload and a PATH-front
   activation grant — where the cited precedent, `[[trust.policy]]`, additionally has `system_locked`
   admission authority and operator-over-project tiering. Demonstrated red+green: unpinned source ⇒
   `[shell.consent]` **absent** from the merged config; digest-pinned source ⇒ **present**.

**Extended by [A-33](./adr_shell_env_addenda.md)** — the gate is **managed-tier-only**. A file named by
`--config` / `OCX_CONFIG` is a **third consent-bearing channel** and the digest gate does **not** apply
to it: `guard_managed_sigstore_trust` is called from inside `fold_managed_tier` alone and gates on
`source.digest().is_none()`, which for a file with no `[managed] source` is not merely skipped but
**undefined**. Nor does `OCX_NO_CONFIG=1` prune it — that flag empties the discovered chain and
suppresses the managed fold but never touches `explicit_paths`
(`crates/ocx_lib/src/config/loader.rs:145-157`, `:327-331`). **Only `OCX_NO_HOOK=1` makes a shell
wholly inert.** The threat class is the same already-out-of-scope one as `OCX_CONSENT_*`: a hostile
parent process.

---

#### C-035 — Config JSON-schema generation must be exercised by a test
**ADR:** Component Contracts (`schemars::JsonSchema` derive; "regenerate `config/v1.json` via
`task schema`") — **plus a discovery correction the ADR does not carry.**

**The gap, verified:** `website/src/public/schemas/` is **gitignored and generated**, not checked in
(`website/.gitignore:18-19`). Worse, config-schema generation is **not exercised by PR CI** —
`verify-basic.yml` and `verify-deep.yml` run `task schema:generate`, which builds `metadata/v1.json`
only, and **no test anywhere calls `schema_for("config")`**. A broken `ShellConfig` `JsonSchema` impl
would compile clean and pass `task verify`.

**Contract:** ship a test that closes this, owned by the config work package.

- A unit or integration test that calls the config-schema generation path (`ocx_schema`'s
  `schema_for("config")` equivalent) and asserts the produced schema **contains a `shell` property**,
  and that `shell.consent.namespaces` renders through `ScopeSpec`'s hand-written `JsonSchema`
  (`oneOf` string-or-table with one of `include`/`exclude` required), not a derive's `anyOf`.
- **Red state to demonstrate:** remove the `JsonSchema` derive from `ShellConfig` (or point `namespaces`
  at a derived schema) and watch the test fail. A green that cannot be told from "never ran" is not a
  check.

`ASSUMPTION:` this test lives in `crates/ocx_schema` rather than in `.claude/tests` or `test/`.
Justification: `ocx_schema` is the only crate that owns schema generation, the test then runs under
`cargo nextest` inside the existing PR gate with no new CI wiring, and putting it in `test/` would make
it an acceptance test of a build-only crate.

---

### 1.5 Reserved keys

#### C-036 — The reserved-key gate moves to the application seam
**ADR:** Decision 1 ("The gate belongs at the application seam, not the emitter"), Component Contracts
(`ocx_lib::env` row).

**The hole, stated:** `is_reserved_ocx_key` has exactly three call sites —
`ocx_cli/src/options/env_override.rs:170` (`ocx run --env`), `ocx_lib/src/env.rs:1406` (the `OCX_ENV`
decode) and `ocx_lib/src/project/env.rs:158` (project/group `[env]`). **None is in package metadata or
the composer** — `Var.key` is a plain `String` — so package-declared env vars are entirely ungated
against the `OCX_*` namespace, and a published package can declare a constant named
`__OCX_ENV_STATE`, `OCX_CONSENT_NAMESPACES` or `OCX_NO_HOOK`.

This design turns that from a wrong-diff nuisance into a **consent bypass**: the env channel (C-031) is
deliberately additive, so a publisher inside **one** already-consented namespace ships metadata setting
`OCX_CONSENT_NAMESPACES = "*/*"`, it composes into the user's shell at the next prompt, is inherited by
every child process, and silently converts the whitelist into allow-all. `OCX_NO_HOOK=1` is the denial
variant. **The env channel stays** — the owner requires it for devcontainer pre-whitelisting — which is
exactly why the read-path gate has to be unconditional.

**Contract — read path:**
- The `OCX_*` / `__OCX_*` skip lives in **the resolver** — `crates/ocx_lib/src/package_manager/composer.rs`'s
  `resolve_env*` seam — dropping reserved keys from *every* composed source **including package
  metadata**, before any consumer sees them. Every consumer reaches it: `Env::apply_entries`
  (`crates/ocx_lib/src/env.rs:578`), which is how `ocx run`, `ocx exec` and `ocx launcher exec` reach the
  child environment, **and** `crates/ocx_cli/src/conventions.rs::emit_lines` (`:217-256`), which
  dispatches `Entry` → `Shell::export_*` for `ocx env --shell`, `ocx direnv export` and `ocx package env`.
- **Verified defect this closes:** `emit_lines` today gates **only** on `is_valid_env_key` and never
  routes through `Env::apply_entries` or `is_reserved_ocx_key` — so a package-declared `__OCX_ENV_STATE`
  or `OCX_CONSENT_NAMESPACES` is still emitted into the eval'd stream by `ocx env --shell=bash`,
  `ocx direnv export` and `ocx package env`, the exact consent bypass this contract opens by naming.
  `emit_lines`'s own `is_valid_env_key` gate does **not** cover this and must not be mistaken for it.
- **Required test:** for every one of `emit_lines`'s three call sites (`ocx env --shell`,
  `ocx direnv export`, `ocx package env`), a package-declared reserved key never appears in the emitted
  stream.
- The alternative of widening `Env::apply_ocx_config` to a whole-namespace strip is **rejected**: that
  function forwards ocx's *own* config keys and is not on the package-metadata composition path at all,
  and a whole-namespace strip there would strip `__OCX_ENV_STATE` from a child env — breaking the
  nested-shell ledger inheritance the exception below exists to protect.
- **Skip is warn-once per compose, never an error** — already-published artifacts must keep resolving.
- `is_reserved_ocx_key` gains a **fourth** call site; its three existing ones are unchanged.

**One deliberate exception:** `Env::apply_ocx_config` does **not** strip `__OCX_ENV_STATE` from a child
env — `ocx run -- bash` must hand the nested shell a consistent ledger.

**`OCX_ENV` is untouched** — name, help text, codec, and the strip at `env.rs:498-502`. Its rename
(superseded Decision 8 / OD-4) is **withdrawn**: `ocx-sdk-python` documents it as opaque pass-through,
aborts startup on a malformed value, rejects `OCX_*`/`__OCX_*` keys at exit 64, and **snapshots ocx's
reserved-key help sentence verbatim in seven golden fixtures**; `rules_ocx` deliberately omits `OCX_ENV`
from `_OCX_NEUTRALIZED_ENV` because ocx strips an inherited one on every compose. Renaming it, rewording
its help, or changing the stripping breaks three downstream things mechanically and buys nothing here.

---

#### C-037 — `ocx package create` rejects reserved metadata env keys, exit 65
**ADR:** Decision 1 (write path), Component Contracts (Exit codes row), Migration table.

**Write path (hard rejection):** `ocx package create` refuses metadata declaring an env key in the
`OCX_*` / `__OCX_*` namespace. This is a **validation** failure and takes the existing **65**
(`ExitCode::DataError`) used for invalid package input. No new exit-code variant.

Home: the `crates/ocx_lib/src/package/metadata/validation.rs` seam, beside the shipped sibling validators
`validate_env_modifier_types` / `validate_env_list_entries`, reached by `ValidMetadata::try_from` /
`validate_for_publish`.

`ASSUMPTION:` the check is a new sibling validator in `validation.rs` rather than a check inside
`package_create.rs`. Justification: that module already owns every other env-shape refusal for the
publish path, and the ADR's read-path/write-path split needs the two to live in different layers.

**Read path stays permissive** (C-036): a published package carrying such a key keeps resolving, its key
skipped with a warning. This is the one hard backward-compat exception this repo keeps.

Breaking-change subject for the changelog:
`feat(package)!: refuse reserved OCX_* env keys in package metadata`.

---

### 1.6 Enablement and the CLI surface

#### C-038 — `options::Hook` and the five-rung ladder
**ADR:** Decision 5 (whole decision).

```rust
// crates/ocx_cli/src/options/hook.rs — new file, flat layout, template: options/completion.rs
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Hook {
    #[clap(long = "hook",    overrides_with = "no_hook")] hook: bool,
    #[clap(long = "no-hook", overrides_with = "hook")]    no_hook: bool,
}
impl Hook {
    pub fn enabled(&self, interactive: bool, configured: Option<bool>) -> bool;
}
```

**Ladder, in this order:**
1. `--no-hook` → false
2. `--hook` → true
3. `OCX_NO_HOOK` truthy → false
4. `[shell] hook` (`configured`) → as set
5. auto: `interactive`

`overrides_with` both ways gives POSIX last-wins; combining the flags is **not** an error (the
`git --[no-]verify` idiom, and this repo's standing paired-toggle convention).

**Boolean `OCX_NO_HOOK`, not tri-state `OCX_HOOK=0|1|auto`.** Every environment toggle in this codebase
is a negative-only `OCX_NO_*` flag: `OCX_NO_COMPLETIONS`, `OCX_NO_CONFIG`, `OCX_NO_MODIFY_PATH`,
`OCX_NO_PROJECT`, `OCX_NO_VERIFY`, `OCX_NO_CONFIG_REFRESH`. A tri-state would be the only one and buys
nothing: the positive channel already exists as `--hook`, and "auto" is precisely what an unset variable
means.

**Correction, carried from discovery — how `OCX_NO_HOOK` is read.** `OCX_NO_COMPLETIONS` is read as a
**bare string literal** via `env::flag("OCX_NO_COMPLETIONS", false)` inside
`crates/ocx_cli/src/options/completion.rs:44` — it is **not** in `ocx_lib`'s `env::keys`. `OCX_NO_HOOK`
follows that precedent exactly: `env::flag("OCX_NO_HOOK", false)`, no `env::keys` entry. Do not
"harmonize" it into `keys` — that would be an unrequested change to a shipped module for one new var.

`env::flag` semantics are the shipped `BooleanString` contract: truthy `1|y|yes|on|true`, falsy
`0|n|no|off|false` (case-insensitive), any other non-empty value **warns and falls back to the default**.

**Interactivity is decided shell-side and passed explicitly**, through a hidden paired toggle `--interactive` / `--no-interactive` (`options::Interactive`, `overrides_with`, POSIX last-wins), which every shim emits from the test its own language provides: `case "$-"` on POSIX, `status is-interactive` on fish, `[Console]::IsInputRedirected` on pwsh, `?(test -t 0)` on elvish. Nushell is exempt — it never invokes `self activate`, applying the global env as JSON data instead.

**The pair feeds rung 5's input and is not a rung.** `Interactive::resolve(probed) -> bool` answers "the flag if given, else the probe", and its result is the `interactive` argument to `Hook::enabled` / `Completion::enabled`. It must not be spelled as `--hook`: rung 2 outranks `OCX_NO_HOOK` and `[shell] hook`, so a shim declaring interactivity there would revoke both opt-outs for every shell it starts.

**No descriptor answers this from inside the binary**, in either direction. Every shim runs the activation with stderr redirected to `/dev/null`, so a stderr probe reads `false` in every real shell — the zero-hooks outage. stdin is wrong the other way: `ssh -t host 'bash -lc "…"'` allocates a pty for a shell that reads the login profile and exits without ever rendering a prompt (measured: `$-` carries no `i`, both stdin and stderr are terminals, and a probe-decided activation emits 5 hook lines into a shell that will never show a prompt, leaking a `mktemp` stamp per invocation). A terminal-parented `bash -l -c` — any `make` recipe — has the same shape. Emacs `M-x shell` is the mirror case: a genuinely interactive session driven over pipes on all three descriptors, which a probe calls non-interactive and the shell's own `$-` calls interactive.

**`shell_is_interactive()` remains as rung 5's fallback**, unchanged, for the two callers that send no flag: a direct `ocx self activate` typed at a prompt, and a shim written by an older `ocx self setup` that has not yet been refreshed. That is rung 5's existing behaviour, not a compatibility shim, and it is deliberately biased towards `true` (`stdin.is_terminal() || stderr.is_terminal()`) because on that path no shim has spoken.

**Default: on, interactive shells only**, from the first release that carries the hook. Fleet blast
radius accepted by the owner (OQ-2).

**Producing obligation, verified gap.** `Hook::enabled(&self, interactive, configured) -> bool` returns a
bare bool today, yet C-050 requires `ocx shell state` to name "which rung decided it". `Hook` therefore
gains: alongside `enabled(...) -> bool`, a sibling accessor reporting **which of the five rungs decided**
(a small enum), so `ocx shell state` reads the decision rather than re-implementing the ladder — a second
implementation of a security-relevant enablement decision in the CLI crate is the failure this closes.
`Completion::enabled` gains the same.

---

#### C-039 — `Completion::enabled` gains `configured`, with the identical ladder
**ADR:** Decision 5 ("`[shell] completions` — the rung completions never got").

```rust
// crates/ocx_cli/src/options/completion.rs — signature change
pub fn enabled(&self, interactive: bool, configured: Option<bool>) -> bool;
```

**Ladder, identical to C-038 in order and in spelling:**
1. `--no-completion` → off
2. `--completion` → on
3. `OCX_NO_COMPLETIONS` truthy → off
4. `[shell] completions` → as set
5. auto: `interactive`

The auto arm is unchanged from what ships: the shim passes an explicit flag from its own interactivity
check where it has one; where it does not, the binary falls back to `std::io::stderr().is_terminal()`.

Why it rides this ADR: completions ship a flag and an env var and **no config rung at all**. Shipping a
second `[shell]` key with a shorter ladder would make the section's grammar unlearnable — a user who has
just written `hook = false` has every reason to expect `completions = false` to exist. One field, one
parameter, one match arm.

**Correction, carried from discovery — the blast radius is one line.** `Completion::enabled` has
**exactly one call site**: `crates/ocx_cli/src/command/self_group/activate.rs:102`. The three existing
unit tests in `completion.rs` are the only other callers.

**Read at the same point as `hook`** (C-044): once per shell start, inside the single `ConfigLoader`
pass — never on the per-prompt path, which reads no config at all.

---

#### C-040 — `ocx self setup --[no-]hook` / `--[no-]completion`: home-tier `toml_edit` write
**ADR:** Decision 5 (tier and mechanism) — **with a discovery correction that changes the contract.**

Grammar added to `ocx self setup`:

```
ocx self setup [VERSION] … [--hook | --no-hook] [--completion | --no-completion]
```

- `--hook` / `--no-hook` writes `[shell] hook = true|false`.
- `--completion` / `--no-completion` writes `[shell] completions = true|false`. **`setup` flattens
  neither pair today** — both are new there.
- **Flag absent writes nothing**, and the default applies.
- **Target: the home tier** — `file_structure.root().join("config.toml")`, i.e. `$OCX_HOME/config.toml`.
  **Not** `ConfigLoader::user_path()` (`config_dir()/ocx/config.toml`).
- A **missing file is created** with just the one section.
- `--config` / `OCX_CONFIG` names a **read** override and never redirects this write. If a higher tier
  already sets the key, the write still lands and the CLI says **which tier will win** (C-034).

**Correction — the mechanism is genuinely new; do not copy `--managed`'s.** The ADR cites `setup.rs:389`
for the home-tier write. That line is in **`crates/ocx_lib/src/setup.rs`** (not the CLI crate), and the
shipped `--managed` write is **not** a `toml_edit` edit: it reads the whole file as a string
(`read_to_string_or_empty`) and drives a **fenced-block state machine** via `crates/ocx_lib/src/setup/rc_block.rs`
— `toml::to_string` of the whole `[managed]` table emitted inside a labeled fence
(`rc_block::MANAGED_LABEL`), classified `Fresh`/`Current`/`FormatUpgraded`/`Dirty`, exit 82 on user
edits. So the shipped write shares only the **target path** with what `[shell]` needs.

**Contract for the new write:**
- **Surgical `toml_edit` edit** (`toml_edit = "0.25"` is already a workspace dependency used by
  `ocx_lib`), **not** a whole-file rewrite and **not** a fenced block. `Config` derives `Deserialize`
  only, so a serde round-trip is not available; a rewrite would discard comments and unknown keys the
  forward-compat contract exists to preserve; and a fence would make `[shell]` an ocx-owned region a user
  may not edit, which is the opposite of the intent for a user-facing toggle.
- Set exactly the one key under `[shell]`, creating the table if absent, preserving every other byte of
  the file.
- **Exit 82 does not apply.** There is no fence, so there is no dirty state. A user's hand-written
  `[shell] hook = false` is simply overwritten by an explicit `--hook`, which is what the flag means.

`ASSUMPTION:` the writer is a **new** module (`crates/ocx_lib/src/setup/shell_config.rs`) rather than an
addition to `setup.rs`. Justification: keeps the fenced-block writer and the surgical writer from sharing
a file whose reader would then have to hold two mental models — and it gives the parallel plan a
file-disjoint owner (§4, WP-8).

---

#### C-041 — `ocx self activate --[no-]hook` and the hidden `--reconcile`
**ADR:** Decision 5 (Delivery layers 1–2), Component Contracts.

Grammar added to `ocx self activate`:

```
ocx self activate [--shell[=NAME]] [--completion|--no-completion] [--hook|--no-hook] [--reconcile]
```

- `--hook` / `--no-hook`: flattened `options::Hook` (C-038).
- `--reconcile`: **hidden**, per the `launcher exec` precedent. The per-prompt entry point.
- `--reconcile --format json` returns the `Plan` (C-011) for the nushell channel (C-048).

**`--reconcile` is a cross-version contract, not plumbing.** A running shell's hook body was emitted by
binary X and invokes whatever `current` points at *now* — which `self update` swaps underneath it, and
which a downgrade or a rollback can make **older**. After a rollback to a pre-hook binary, every prompt
in every open terminal execs a binary that has never heard of `--reconcile`.

**The emitted hook resolves the binary through `current`, unconditionally; `OCX_BINARY_PIN` has no
effect on the `--reconcile` call.** State it here rather than leaving it inferable: none of the five
`env.*` shim bodies reads the pin — each hardcodes the `current` symlink path
(`crates/ocx_lib/src/setup/shims.rs:37`, repeated per family). The pin has exactly three consumers, and
the shim is none of them: the Windows `.exe` shim, the script host's `ocx` module, and generated Unix
launcher bodies — all *re-entrant/downstream* invocations where a running ocx pins a child back to its
own `current_exe()`. The interactive shell's own top-level resolution is upstream of that mechanism and
structurally cannot consult it. Rollback and downgrade remain real; the pin does not.

Two rules make that harmless, and both are cheap:
1. The emitted hook **discards the reconcile call's stderr and ignores its exit status**, so an
   unknown-flag clap error is invisible rather than printed once per prompt.
2. The emitted hook **probe-guards the binary** — if the resolved absolute path is missing or not
   executable, the hook is a **silent no-op**.

That probe guard is the mechanism this ADR's Amends line promises to
`adr_idempotent_path_move_to_front.md`; it lives here, **with an acceptance test**, not only as an NFR
adjective.

**Enablement is not re-evaluated per prompt.** `--reconcile` **bypasses `Hook::enabled` entirely**: it
runs in a fresh process with no `configured` value, and reading one would violate the zero-config rule
(C-044). Consequence, stated so it is discovered here and not in a bug report: **`OCX_NO_HOOK=1`
exported mid-session takes effect at the next shell start, not the next prompt.**

---

#### C-042 — Where the config rung is read, and the negative-consent cache
**ADR:** Decision 5 (Where the config rung is read; the unconsented-project bullet).

**Option C**: `ocx self activate` reads `[shell]` — both `hook` and `completions` — **once at shell
start**, through one `ConfigLoader::load_with_local_view` pass.

Rejected and recorded: (A) baking the decision into the shim body (shims are byte-identical across
installs with no substitution, guarded by `no_shim_contains_a_substitution_placeholder`); (B) the RC
block exporting `OCX_NO_HOOK=1` before sourcing (an exported toggle leaks into every child process —
the exact failure the `OCX_ACTIVATED` removal exists to prevent); (D) flags + env only (fails the
owner's `self setup --[no-]hook` requirement).

Cost, stated honestly: three `symlink_metadata` probes, up to three small TOML parses, one
managed-snapshot read. Real, bounded, **once per shell**. It does **not** promote `self activate` to
`Context::try_init` — no OCI client, no `OciIndex`, no `PackageManager`. Use the existing loader, never
a bespoke mini-parser: a second precedence implementation is a second source of truth.

**Per-prompt discipline is absolute.** The per-prompt path reads **no config at all** — flags, env,
ledger fingerprint and watch-set stats only. Config is loaded only once the fingerprint has already
decided a recomposition is needed.

**The unconsented project needs a negative cache, or that rule is false in its most common case.**
Evaluating consent reads `[shell.consent]` *and* parses the lock — a full loader pass on **every prompt**
for a user sitting in a fresh clone, which is exactly the state this design calls normal. So the ledger
carries `verdict: "inert"` alongside `fp` (C-002): when the fingerprint is unchanged **and** the cached
verdict is `inert`, the prompt is stat-only and reads nothing.

**Only the negative verdict is cached.** An `Activate` verdict is always re-derived, never read from the
carrier — caching it would make the ledger a consent input, which C-007 forbids. Caching `inert` can only
ever cause ocx to do *less*: the fail-safe direction, matching the narrow-never-widen posture.

**Extended by [A-13](./adr_shell_env_addenda.md)** — the cache is only sound because the watch set it
hangs off **expires** it: the fingerprint's watch set gains the ledger-recorded config-tier paths (home
`config.toml`, user `config.toml`, and the `OCX_CONFIG` / `--config` override if one was in effect,
each with presence) and the project's consent stamp, and `fp` folds the raw `OCX_CONSENT_*` values
(C-019). Without those members a grant added from another terminal never takes effect until the shell
restarts. The per-prompt path still reads **no config** — it stats the *recorded* paths, discovered on
the last `ConfigLoader` pass.

---

### 1.7 Shell-side emission

#### C-043 — Per-shell hook registration, append-only
**ADR:** Decision 5 (Delivery layer 1).

**Existing emission order is fixed and load-bearing**: completions **first** (PowerShell's
`using namespace` must be the first statement `Invoke-Expression` sees), then PATH prepend, then global
env. **Hook and wrapper emission append after those.**

**Append-only, never clobbered — for every shell, not just PowerShell.** The same points are owned by
starship, oh-my-zsh, powerlevel10k and direnv, and clobbering them is the single most common integration
bug in this category.

| Shell | Mechanism | Hard requirement |
|---|---|---|
| **bash** | append to `PROMPT_COMMAND` | Handle **both** the string form **and** the Bash 5.1+ **array** form. Preserve `$?` across the hook. |
| **zsh** | `add-zsh-hook precmd` (or `precmd_functions+=`) | **Never** define `precmd()` — that is how starship avoids clobbering. |
| **fish** | a named `--on-event fish_prompt` function | Additive by construction. |
| **PowerShell** | **wrap** the existing `prompt`, calling through to the captured previous definition | The only extension point pwsh has; every surveyed tool wraps it. |
| **nushell** | **append** to `$env.config.hooks.env_change.PWD` | **Never assign** it. |
| **elvish** | append to `$edit:before-readline` via `[$@edit:before-readline { … }]` | The whole registration rides inside `eval` of a string — `edit:` is bound only in an interactive elvish and elvish resolves every variable in a chunk before executing any of it, so a direct reference is a compile error that kills the unit and any `try` around it. |

The bash array form is the specific footgun: [Warp#5219](https://github.com/warpdotdev/Warp/issues/5219)
(syntax errors on semicolon-terminated elements) and
[vscode#158090](https://github.com/microsoft/vscode/issues/158090) (exit-code `$?` clobbered by
string-vs-array confusion).

**Elvish's guard carries no watch-set term, and that is the arm's whole deviation**
([#341](https://github.com/ocx-sh/ocx/issues/341), amended 2026-08-25). Elvish 0.21 exposes no file
timestamp — `os:stat` returns `name`/`size`/`type`/`perm`/`special-modes`/`sys` and documents that
timestamps are not exposed — and ships no clock module, so there is no stamp and nothing to compare a
watch member against in-shell. Its guard is the two terms elvish can evaluate for free: an empty carrier
and a changed `$pwd`. Buying the missing term with an external `test -nt` would put one exec on every
quiet prompt, which is exactly what C-044 exists to remove, so it is not bought. The wrapper compensates
by **invalidating** the recorded directory rather than calling the guard (C-045's handover, inverted for
this arm). Clearing the recorded directory does not avoid the reconcile — it guarantees one at the very
next prompt, the same single reconcile an inline guard call would have run. What it buys is *when*: the
work leaves the ocx command's critical path and lands on the prompt, so `ocx version` keeps its direct
cost. The next prompt is also what C-045 already names as the correctness floor.

**The residual, stated generally rather than by one example of it**: any watch-set member (C-019) that
changes without an `ocx` invocation *in this shell* and without a `cd` reconciles only at the next `cd`
or the next ocx invocation in this shell, not at the next prompt — an `ocx.toml` edited by hand in place
is one instance, alongside `ocx add --global` run in another shell, a `git checkout` that rewrites
`ocx.lock`, a hand-edited `~/.ocx/config.toml`, a managed-config snapshot refresh, and `ocx self update`
run from another shell. The wrapper goes in through `edit:add-var` because a `fn` defined inside an
`eval` unit does not escape it. **Registration idempotency keys on the shell, not the process** —
`__OCX_ENV_HOOK` does not exist: the marker is the registered closure's **rest-parameter name**
(`{|@__ocx-prompt-hook| … }`) and the probe reads each closure's parsed `arg-names` out of
`$edit:before-readline`, which is the only per-shell store an `eval` unit can both write and read, so a
shell that replaces its own image with `exec elvish` still re-registers rather than reading a stale
marker back out of its inherited environment. The `to-string` substring scan this replaces was deleted
as a **silent-suppression bug** (A-42): `to-string` renders each closure with its literal body *and* the
whole source of the `eval` unit that defined it, so a user's own hook that merely **mentioned** the
marker in a comment made the probe true and no ocx hook registered at all, for that shell's whole life.
Text cannot forge an entry in a list the parser produces. The one
process-environment variable that remains, `__OCX_ENV_PWD`, is a **pid-qualified composite of every term
the elvish guard can evaluate without spawning a process** — `<pid> <pwd> <DIRENV_DIR> <MISE_SHELL>
<__MISE_ORIG_PATH>`, space-joined, an unset sentinel contributing an empty field (`hook.rs`'s
`elvish_pwd_value`; documented as interface in `website/src/docs/reference/environment.md`). Not a bare
`$pwd`: the pid is what keeps a nested elvish's inherited environment from reading as an
already-reconciled match, so its own first prompt reconciles too, and the three-sentinel tail is A-36's
yield term, which rides in this value because elvish has no shell-local an `eval` unit can both write and
read (A-43). The key's name is narrower than its contents on purpose — the value answers *"has anything
the guard can see moved?"*, of which the directory is one term.

**PowerShell 5.1 is supported, not degraded** (OQ-3). The missing `LocationChangedEventArgs` (PS 7+
only) affects exactly one case: a **programmatic `Set-Location` that never returns to a prompt** — the
non-interactive scripting path this hook should not serve. Interactively a `cd` is always followed by a
prompt, the wrapped `prompt` fires, and fidelity is **full**.

**Extended by [A-22](./adr_shell_env_addenda.md), [A-24](./adr_shell_env_addenda.md),
[A-36](./adr_shell_env_addenda.md)** — the pwsh hook body is wrapped in `try { … } catch { }` in full,
with `$ErrorActionPreference = 'Continue'` and `$PSNativeCommandUseErrorActionPreference = $false` set
in the hook's **own scope** around the reconcile invocation and restored in a `finally`, and with `$?`
and `$global:LASTEXITCODE` captured on entry and restored on exit: under the common
`$ErrorActionPreference = 'Stop'` hardening pair a native command writing to stderr raises a
**terminating** error and `2>$null` does not prevent it, so discard-and-ignore alone does not achieve
the "never break a prompt" rule (A-22). The nushell hook is appended with `++` onto
`($env.config.hooks?.env_change?.PWD? | default [])` and assigned back — never `=` onto `.PWD`, never
`$env.config.hooks = { … }` — with **every** intermediate level defaulted (`| default {}`) so a `nu -n`
session carrying no `hooks` key does not error and take the PATH prepend down with it; the appended
element is a **closure**, and the body must run **after** the user's `config.nu`, which the
`$nu.vendor-autoload-dirs` slot provides as a tested contract (A-24). Hook-registration order relative
to direnv's or mise's own entries is **unspecified and accepted**: any apply-then-revert flap is
bounded to **one prompt** and self-heals — no reordering logic, no cross-tool coordination, no retry
(A-36).

---

#### C-044 — The shell-side zero-exec short-circuit
**ADR:** Decision 5 (Delivery layer 2), NFR Latency.

**The no-op path must not exec ocx at all where the shell can decide it.** `crates/ocx_cli/src/main.rs:18`
is `#[tokio::main]`: every invocation builds a multi-thread Tokio runtime before dispatch reaches any
`stat`, measured at **~3.8 ms** for `ocx --version` on a warm 32-core Linux box — most of a 5 ms budget
spent learning that nothing changed, and worse on macOS, on a 2-core runner, and above all on Windows,
where process creation is an order of magnitude dearer and where this design calls PowerShell
first-class.

**Contract:**
- The emitted hook **carries the watch-set paths** and short-circuits shell-side. bash/zsh/ksh
  `[[ file -nt stamp ]]` is a builtin mtime comparison with **zero exec**. **Elvish carve-out**:
  `elvish_registration` takes no `watch_paths` parameter at all — elvish 0.21 has no in-shell mtime, so
  there is nothing to carry (C-043).
- `ocx self activate --reconcile` is invoked **only when something is newer**. **Elvish carve-out**:
  "newer" has no elvish meaning; its guard fires on carrier-empty OR a changed `$pwd` instead (C-043).
- Shells without a builtin newer-than test **fall back to the exec on every prompt** and are budgeted
  separately. **Elvish is not this case either**: its guard still costs zero execs on a quiet prompt —
  `==s`/`!=s`/`$E:`/`$pwd` are all builtins — it simply has one fewer term to test than the watch-guarded
  arms (C-043).

**The benchmark contract** (this is a check, so it needs a red state):
- The CI assert is **per-platform**, stated as `exec_floor + Δ`, and **the floor is measured in the same
  job**, never assumed or imported from another tool's benchmark.
- **`Δ ≤ 2 ms`, restored — the 2026-08-25 amendment is withdrawn as a misdiagnosis, not re-affirmed**
  ([#340](https://github.com/ocx-sh/ocx/issues/340)). That amendment measured 14.3–16.7 ms of overhead on
  the applying path and, reasoning that the reconciler "constructs the package manager, and every command
  that does pays the same," widened the reconcile side to `Δ ≤ 25 ms`. The overhead was real; the
  diagnosis was wrong. It was never the reconciler — it was `HostCapabilities::detect_and_cache`'s
  loader-directory walk (roughly 7,800 per-entry `tokio::fs::file_type().await` hops) running on a path
  whose result never changes a byte of the emitted stream. That walk is now one `spawn_blocking` over
  `std::fs` against canonical-deduped scan roots, with the result persisted at
  `$OCX_HOME/state/host/capabilities.json` under a 1-hour TTL, so the common case pays one file read
  instead of one directory walk. **That file is a versioned on-disk format and therefore an interface;
  its contract is [A-41](./adr_shell_env_addenda.md)** — the record holds *evidence*
  (`{ version, loaders }`, one entry per loader that classified, with `os.features` **derived** from it,
  so a claim no recorded loader produced is unrepresentable rather than merely refused); freshness is a
  per-loader device/inode/size/mtime identity, not path existence; and a detection that classified
  **nothing** is never persisted, because an empty loader list is *vacuously* fresh and would otherwise
  answer `os.features` with the empty set for the whole TTL — turning one degraded probe into an hour of
  `FeatureMismatch` (exit 65) on every install. Re-measured, N=200 interleaved spawns (min / p50 / p95, ms): floor
  `ocx version` **3.467 / 4.000 / 5.251**; reconcile before the fix **20.455 / 22.560 / 28.191**;
  reconcile after **4.771 / 5.600 / 7.108**. The Δ this gate actually asserts — min-of-15 reconcile minus
  min-of-15 floor — goes **17.935 ms → 0.991 ms**, comfortably inside the original `Δ ≤ 2 ms`, and the
  emitted stream is byte-identical across the fix (`sha256 e60ba431…`, 3111 B) — the fix changes cost,
  never output. The false "a second `ConfigLoader` pass reds it" justification is withdrawn with it: one
  bounded pass measures 0.3–1.1 ms, nowhere near either budget. The dropped-`--offline` fault-injection
  figure is corrected from 31.5 ms to the re-measured **47.9 ms** — still comfortably clear of
  `Δ ≤ 2 ms`, so it remains a valid target. **This is an assert, not a warning** — a stated-but-unenforced
  number is a spec claim no reader can check.
- Each assert's **red state is produced by a fault injection inside the measured process**, at a seam only
  that measured path reaches: `hook::registration` for startup, `hook::checkpoint` for the reconcile
  (which only `--reconcile` emits). Two injected runs, each naming the gate it must red, because a fault
  injected at one seam does not demonstrate that the *other* gate can also go red — "something went red"
  is not evidence for *this* gate, even now that both share the same `Δ ≤ 2 ms` budget. A wall-clock
  budget on shared runners is otherwise the canonical flaky-or-vacuous gate.
- A separate benchmark shows the shell-side short-circuit costs **zero execs** on the no-op path.
- **The reconciler's fixed point is asserted here too** (C-015 rule 0,
  [#342](https://github.com/ocx-sh/ocx/issues/342)), as a pair: the first fire must emit path applies and
  the steady-state fire after it must emit none. The first half is not decoration — without it an arena
  whose project never composed passes "steady == 0" while proving nothing, which is exactly the state the
  measurement was in before it was gated (a 95-byte stream from an unconsented project).
- Shell startup gains one bounded `ConfigLoader` pass (C-042); measure it and **record the number in the
  plan**.

---

#### C-045 — The wrapper function, and "no emitted snippet may ever call bare `ocx`"
**ADR:** Decision 5 (Delivery layer 3, and the paragraph after it).

**The wrapper is a latency optimization for same-command-line chaining, never the correctness floor.**
A shell function named `ocx` runs the real binary and then fingerprint-checks before returning, so
`ocx update && cmake --build .` or `ocx add --global foo && foo` sees the new env **within one command
line**, with no prompt in between. It is the only possible host for that case — a child process can never
mutate its parent's environment — and that case is *all* it buys.

**The correctness floor is the next prompt and needs no wrapper at all.** Every way of escaping the
function name **degrades to next-prompt correctness rather than breaking**: an absolute-path invocation,
`command ocx`, `\ocx`, `$(which ocx)`, and any invocation from a script, a Makefile or a subshell. None
is a supported-vs-unsupported distinction. Interactive shells only; scripts hit the real binary and use
`ocx run`.

**Hard rule — no emitted snippet may ever call bare `ocx`.** The wrapper is named `ocx` and
`command -v ocx` finds functions, so a bare call inside the emitted stream would execute the wrapper
inside a command substitution and **capture its output into the env stream**. Every ocx-emitted call site
uses the resolved **absolute binary path** (the `$_ocx_bin` pattern the shims already use).

Two test obligations: a **grep on the emitted text** (tier 1 — no emitted line contains a bare `ocx`
invocation), and **five named behavioural cases**, one per escape form, each asserting
degrade-to-next-prompt-correctness rather than breakage — every case runs with the wrapper function
*defined* in the shell:
1. absolute-path invocation;
2. `command ocx`;
3. `\ocx`;
4. `$(which ocx)`;
5. invocation from a script, a Makefile, or a subshell.

**Extended by [A-35](./adr_shell_env_addenda.md)** — the wrapper MUST capture the real binary's exit
status **immediately after it returns, before running any other command including the fingerprint
check**, and MUST return exactly that value; the check runs purely for its side effect and never
influences the returned status. An optimization that silently changes `$?` breaks
`ocx add --global foo && foo` in exactly the way the wrapper exists to make safe. Red state: drop the
captured-status return and `$?` becomes 0 after a failing subcommand.

**Elvish's form of A-35 is structural, not a captured-and-returned value — elvish has no `$?`.** A
non-zero exit from the wrapped binary is an **exception**, and A-35's letter translates to: the `defer`
that clears `__OCX_ENV_PWD` is registered *before* the wrapped call, so it still runs on the way out when
the call raises, and the wrapped call is the wrapper's **last statement**, so nothing of the wrapper's own
runs after it to catch, swallow or rewrite the exception. Pinned by `hook.rs`'s
`the_elvish_wrapper_leaves_the_real_binarys_failure_alone` test, which asserts both orderings directly:
the wrapped call ends the emitted text, and the `defer` registration precedes it.

---

#### C-046 — Output channel and `set -u` discipline
**ADR:** Decision 5 (final paragraph), D3.

- **All user-visible hook output is `printf … >&2` *inside the eval'd script*** — never the binary's
  stderr. The shims discard the binary's stderr (`2>/dev/null`), so the script body is the only reliable
  channel on the startup path. This is also the channel C-034's managed-strip warning must use.
- **`set -u` discipline is binding**: every ledger read uses default expansion (`${__OCX_ENV_STATE-}`
  and per-shell equivalents), because the variable is unset on the first prompt **by construction**.
- **The hook path always exits 0** (C-051).

**Extended by [A-21](./adr_shell_env_addenda.md)** — this channel needs a **new per-arm primitive**,
`Shell::emit_message(text) -> Option<String>`, returning `None` for `Batch` (it hosts no hook). None
exists today: `Shell::comment` produces a comment, which prints nothing. Every message — summary,
inert-project hint, over-cap abandonment, direnv/mise yield, managed-strip reason — goes through it,
passes **that arm's own value escaper**, and rides as a `printf` **format argument, never the format
string** (`printf '%s\n' '<escaped>' >&2` on the POSIX arms). And no message is emitted on the
**startup** path at all — every one is deferred to the first `--reconcile` run (C-051).

---

#### C-047 — The thin-dispatcher invariant, and a check that actually enforces it
**ADR:** Decision 6.

**Invariant (state it, do not revisit it).** The `env.*` shim bodies are **pure dispatchers with no ocx
business logic**. Four of the five families resolve `$OCX_HOME`, find `ocx` through the `current`
symlink, and `eval`/`source`/`slurp` the output of `ocx self activate`, generated fresh by the
*currently running* binary at every shell start. **A change to what gets exported therefore requires no
shim rewrite and reaches every new shell immediately.**

**The invariant is currently unenforced, and the test previously cited for it does not enforce it.**
`each_shim_resolves_the_binary_through_the_current_symlink` (`shims.rs:445,461-464`) asserts
`body.contains("self activate") || body.contains("@('self', 'activate', '--shell=powershell')") ||
body.contains("--format json --global env")` — i.e. *"invokes the binary"*, not *"is a pure dispatcher"*.
A shim that inlined arbitrary reconciliation logic passes it unchanged; **`ENV_NU` already does**, and
the third `||` arm exists precisely to let it.

**Contract — add the check that would enforce it:**
- A per-family **body-size ceiling** (a named constant per family, set from current sizes plus headroom)
  **plus** a **denylist of business-logic tokens** (`consent`, `trust`, `ledger`, `reconcile`, `priors`,
  `__OCX_ENV_STATE`, and the whitelist var names) over the **four eval-capable families**.
- **Nushell is explicitly exempted** by Decision 6(b) — it inlines the apply by necessity.
- **Demonstrated red** by inlining one such token into an eval-capable family's body.
- Cite the existing test for what it proves (invokes the binary), not for what it does not.
- **Strip comments before scanning** — a denylist that quotes the forms it forbids matches its own
  comment.

**Lag lives in exactly three places and nowhere else:**

| Lag surface | Mechanism | Size |
|---|---|---|
| (a) Shim / RC **wrapper body** changes | `refresh_shell_integration_after_swap` runs in the **old binary still in memory** (`update.rs:109-111`), so the hop that swaps in a binary carrying a new body does not heal on that hop; the heal lands on the next `self update` or a `self setup` re-run. Diff-gated (`needs_write`), best-effort, never fails the update | rare, small |
| (b) **Nushell — the whole activation body** | `env.nu` does not call `self activate` at all; for nushell every activation-logic change **is** a body change, so it lags per (a) | **one hop, every change** |
| (c) An already-running shell | Evaluated its activation once at startup; a running shell cannot un-define a function it already sourced | universal |

The earlier "hook logic lands one `self update` later" framing is **false for this codebase** and must
not resurface in any plan, doc, or commit body.

`rc_block.rs` already solves the harder half better than conda — a fence carrying a format version and a
content hash driving a `Fresh`/`Current`/`FormatUpgraded`/`Dirty` state machine, exiting 82 on user
edits. Prior art to cite, not reinvent.

*Skipped:* a `# ocx-shim-schema: 1` marker in the shim body for observability. No thin-stub tool needs
one. Add it if `ocx about` ever needs to flag a stale shim.

**Extended by [A-24](./adr_shell_env_addenda.md), [A-34](./adr_shell_env_addenda.md)** — two properties
of the dispatcher bodies are contracts, not incidentals. `ENV_NU` installs its PWD hook by **appending
with `++`** onto a fully `default`-ed path and from the vendor-autoload slot that runs **after**
`config.nu`, never by assignment (A-24, C-043). And every family **resolves the binary through the
`current` symlink unconditionally** — no body reads `OCX_BINARY_PIN`, and the enforcing check is a
`rust-unit` grep over the five emitted bodies asserting none contains that name (A-34, C-041).

---

#### C-048 — Nushell consumes a JSON `Plan`, and its constant-unset is gated on a spike
**ADR:** Decision 6(b).

`ENV_NU` never calls `self activate`; it runs `^ocx --format json --global env | from json` and applies
with `load-env`. So **nushell takes structured data, not emitted text**:
`ocx self activate --reconcile --format json` returns the `Plan` (C-011) and the inlined nu body applies
it. That is the one place `Plan` needs a JSON wire shape, and it is why
`remove_list_element` returning `Option<String>` has **no nushell consumer** — nushell is not a `None`
arm like Batch, it is a **different channel**.

**Status — the `Plan` JSON channel is PRODUCED but NOT YET CONSUMED.** What ships is the
global-toolchain half on *both* nu paths: the `ENV_NU` startup body and the PWD hook it appends each
re-run `ocx --format json --global env` (`setup/shims.rs`), so no nu body invokes `--reconcile` at all.
`emit_plan_json` (`self_group/activate.rs`) is therefore producer-only, and A-23's `v` gate has no
reader yet. The shipped website page documents exactly that — global toolchain only, no project
reconcile, no revert, no consent gate. **What would consume it**: replacing those two `--global env`
calls in the nu body with the `--reconcile --format json` call, which is WP-12b's work behind the
`hide-env` spike gate below. The contract above is not withdrawn — it is the design that arm lands on,
and it is stated here so a reader does not mistake it for shipped behaviour. From that moment 6(b)'s
one-hop lag starts applying to reconciler changes too; today it applies only to the `--global env` body.

Two consequences the plan must carry:
1. The nu apply body grows, so 6(b)'s one-hop lag applies to **every** reconciler change.
2. **`load-env` cannot unset.** `hide-env` is the only primitive, and its scoping inside hook blocks is a
   known hazard. **Decision 3's constant-revert (`restores: (key, None)`) is unimplementable on nu until
   the spike lands.** Say that rather than claiming parity.

**Gate:** the nushell work package stays blocked on a **red+green spike** proving element removal *and*
unset on a real nushell before any parity claim. The spike is a **tier-2** gate (execute the snippet, no
pty), not tier 3.

Also carried: **do not generalize mise's static-nushell shape to the other shells** — OCX already made
that call correctly.

**Extended by [A-16](./adr_shell_env_addenda.md), [A-23](./adr_shell_env_addenda.md)** — all three
nushell emits use a **plain, non-interpolating** double-quoted literal (`shell.rs:238`, `:412`,
`:439`), so `escape_value`'s nushell arm reduces to `\` → `\\` and `"` → `\"` only and its stale doc
comment (which claims `$"..."` interpolation) is rewritten; if any nu emit ever adopts the `$"..."`
form it gets its own escaper (A-16). And `NU_ENV_APPLY_LOOP` becomes a **four-way** dispatch —
`type == "path"` ⇒ move-to-front prepend, `type == "list"` ⇒ the `export_list` fold on the entry's
effective separator, `type == "constant"` ⇒ `load-env`, **anything else ⇒ skip, applying nothing** —
gated by the `Plan` `v` check (C-011). This is a **live defect, not a forward-compat hazard**: today's
two-way `else { load-env … }` arm applies a `type: "list"` entry as a constant, overwriting a
separator-joined variable instead of folding into it, on a kind that already ships (A-23).

---

#### C-049 — direnv / mise coexistence yield
**ADR:** Decision 9.

**The yield signal is the other tool's live session state, never a file on disk — normative, because the
cheap implementation is the wrong one.**

ocx yields **only when that tool is actually managing this directory**:
- **direnv**: `DIRENV_DIR` set **and naming the resolved project's canonical directory**.
- **mise**: `MISE_SHELL` **or** `__MISE_ORIG_PATH` present.

An `.envrc`, a `mise.toml` or a `.tool-versions` checked into a repo where the tool is **not installed,
not hooked, or not active in this shell** must **not** suppress ocx activation. A config file is evidence
of someone else's workflow, not of a live hook that will set the env at this prompt. Yielding on file
presence would leave the project **silently managed by nobody** — worse than either tool winning, and
undetectable from inside ocx.

**Yield semantics — direnv:** apply the **global** scope only, **revert any project scope already
applied**, print **one info line** naming direnv as the owner. When `DIRENV_DIR` names a *different*
directory, treat it as **absent** — direnv is active for some ancestor, not for this project — and
proceed normally. Fighting direnv is an explicit non-goal; `ocx direnv export` remains supported and
untouched.

**Yield semantics — mise:** symmetric and identical. mise is the other per-prompt PATH-prepending hook,
and its own docs say direnv and mise should not be combined because "incompatibilities are not considered
bugs". Declaring it a non-goal would be defensible; leaving it unmentioned is not, because the collision
is structural.

**Out of scope, recorded so it is not raised as a gap:**
- **tmux / SSH reattach** ([direnv#106](https://github.com/direnv/direnv/issues/106)) is a shell-*init*
  timing bug. This design recomputes D from truth every prompt regardless of L's staleness, so it does
  not transfer.
- **IDE terminals that snapshot env once at launch** (JetBrains, any long-lived process holding
  `JAVA_HOME`) are [#189](https://github.com/ocx-sh/ocx/issues/189)'s class — processes that never
  re-read env.

`ASSUMPTION:` the detection lives in `ocx_lib` (a small `shell::coexistence` module returning a typed
`Yield` verdict), not inline in `activate.rs`. Justification: this repo's standing rule is that the lib
hosts substance and the CLI is a thin wrapper, and `ocx shell state` (C-050) is a **second** consumer of
the same verdict — so a shared seam is required, not speculative.

**Extended by [A-37](./adr_shell_env_addenda.md)** — the two checks are **independent `if`s, never an
`elif` chain**: ocx yields on a matching `DIRENV_DIR` **or** on `MISE_SHELL` / `__MISE_ORIG_PATH`,
regardless of the other's state, and prints **one info line per observed tool**. With both sentinels
set and matching, both lines appear. The three-way case (direnv versus mise ordering) is accepted
as-is — that fight is between those two tools and ocx is out of it either way. Red state: an `elif`
between the two checks silently suppresses the second tool's line.

---

### 1.8 Introspection, exit codes, docs

#### C-050 — `ocx shell state`
**ADR:** Decision 10.

**The one new command surface this design adds.** Everything else is a flag, a config key, a private
variable or a hidden `--reconcile` arm.

```
ocx shell state          # human-readable diagnostics
ocx shell state          # with root --format json → machine-readable, same command
```

Home: `crates/ocx_cli/src/command/shell_state.rs`, registered on the existing `command/shell.rs`
dispatcher beside `shell completion`. Report type in `crates/ocx_cli/src/api/data/`, implementing
`Printable`. `--format` stays a **root/context** concern — no subcommand `--format`, per this repo's
standing no-format-divergence rule.

**What it prints, all derived, none of it mutating:**
- the **decoded ledger** — envelope tag, schema `v`, and the payload **rendered as fields, not as
  base64**;
- what is currently applied **per scope**, `global` and `project` separately;
- **fingerprint status** — the watch set, each member's mtime/size, and whether the recorded `fp` still
  matches;
- whether **`priors` are intact** for each constant the project scope owns — the one datum nothing can
  reconstruct, and the thing C-012's repair gesture destroys;
- and, most importantly, **why the shell is not active when it is not**.

**The enumerated inertness reasons** (each must be individually reachable and individually tested — this
is the command's reason to exist):
1. **no consent stamp and no matching grant** — with the project's derived source set and the `paths` /
   `namespaces` grants it was tested against;
2. **stamp present but the current lock's source set is not a subset of it** — **naming the source that
   is new**;
3. **the hook is disabled** — naming **which rung** decided it (`--no-hook`, `OCX_NO_HOOK`, or
   `[shell] hook = false`) **and the tier that set it**, including the managed tier winning over a
   user's own file;
4. **yielded to direnv or mise** — naming the **live signal observed** (`DIRENV_DIR` and the directory it
   names, `MISE_SHELL` / `__MISE_ORIG_PATH`), because a user staring at an `.envrc` will guess the wrong
   cause;
5. **ledger over cap** — read from the `over_cap` **marker** the carrier still carries (C-004), naming
   the abandoned scope, which is reconciled as absent: the one degradation that loses information
   rather than repairing it;
6. **ledger absent, truncated, or unrecognised envelope tag** — **distinguishing** the *first prompt of a
   shell* (nothing applied, nothing to repair) from a *corrupt carrier* (a scope was applied and its
   record is gone);
7. **the lock's claim would have granted and the store does not corroborate it** — `UncorroboratedNamespace`
   (A-39), showing the **claimed** and **recorded** source sets side by side, because a bare "no grant"
   sends the user to edit a whitelist that already matches. This one outranks reason 2 when both hold: it
   names the clause that was about to grant. It is also the reason that **persists for as long as the store hit
   holds** — a digest already materialized under repository A is a store hit for a lock naming B, so that
   pull returns before `record_origin` and mints no B marker (it is not unconditional: both early returns
   are gated on `check_install_status`, A-39's residual) — so the output says what actually resolves it
   (a stamp, or a `paths` entry), never "re-run `ocx pull`".

**Hard contract — the output is human-readable and must never be eval-able.** This is a rule, not a style
note. `ocx self activate` and `--reconcile` emit shell source whose entire purpose is to be `eval`'d;
this command emits diagnostics whose entire purpose is to be read. A surface where the two are confusable
is one copy-paste away from executing a diagnostic dump in a live shell, and one shim bug away from
`eval`ing a state report into someone's environment. Therefore:
- **no line** of output is valid `export` / `set` / `$env.` syntax in **any** of the ten arms;
- a test asserts the output of the two commands is **never interchangeable**, for **every** enumerated
  inertness reason.

**No background work on the init path.** `ocx shell state` does not run the background update check and
does not require a managed snapshot — it is a diagnostic, and a diagnostic that fails because the thing
it is diagnosing is broken is useless. Today `Self_` is in the update-check skip list and `Shell` is not;
this must change so `ocx shell state` is skipped too.

**Read-only, absolutely**: it never writes a stamp, never repairs a ledger, never emits a plan. Repair is
the `unset` gesture or a new shell (C-012); this command is how a user checks the gesture worked.

**Rejected and recorded so it is not re-proposed: `ocx shell refresh`.** A child process cannot mutate its
parent's environment, so it would have to be either `eval "$(ocx shell refresh)"` — putting the eval
ceremony back in the user's hands for a case the next prompt already handles — or a shell alias/function,
which inherits every fragility the wrapper has **without** the wrapper's justification. The **request-file**
variant (write a marker into `state/projects/<key>/`, consume it at the next prompt) is implementable and
was cut as complexity for no gain: a write surface, a consume-and-delete race, and a third thing the
fingerprint fast path must stat, to serve a case the retirement rule (C-016) and the `unset` gesture
already cover between them.

**Extended by [A-01](./adr_shell_env_addenda.md), [A-12](./adr_shell_env_addenda.md),
[A-26](./adr_shell_env_addenda.md), [A-28](./adr_shell_env_addenda.md),
[A-29](./adr_shell_env_addenda.md), [A-32](./adr_shell_env_addenda.md)** — the over-cap state is read
from the ledger's `over_cap` **marker**, not inferred from an absent carrier (A-01), and the reason
enumeration gains four rows: a **skipped symlinked `ocx.toml` candidate** with the ancestor project
activated in its place, naming `--project` / `OCX_PROJECT` as the opt-in — the loader's `log::warn!`
never reaches the prompt, so this row is the user's only path to that answer (A-12); *"active via
`paths` grant; source-set drift is not tracked for path grants"* (A-26); a **`paths` near-miss**
differing from the canonical directory only by ASCII case or separator style (A-28); and the deciding
config tier **by name**, never a hard-coded "managed" (A-32). This command is also a named non-member
of the six-writer stamp allowlist: running it MUST NOT create `state/projects/<key>/` — it is the
command a confused user is told to run, and a stamp written from here would consent to the very
project it is diagnosing (A-29).

---

#### C-051 — Exit codes and error semantics
**ADR:** Component Contracts (Exit codes row), D3.

| Path | Code | Rule |
|---|---|---|
| The hook path (`self activate`, `--reconcile`, every prompt) | **always 0** | D3 — including an unknown `--reconcile`, whose stderr the emitted body discards (C-041) |
| `Inert` verdict | **0** | **Not an error.** `ocx self activate` exits 0 and emits **no diagnostics at all**; the hint line rides the first `--reconcile` run (see below) |
| `ocx package create` with a reserved `OCX_*`/`__OCX_*` metadata env key | **65** | Existing `ExitCode::DataError`, used for invalid package input (C-037) |
| `ocx shell state` | **0** | Read-only diagnostics; an inert shell is a finding, not a failure |
| `[shell]` in `ocx.toml` | **existing parse-error code** | Refused by `ProjectConfig`'s `deny_unknown_fields`; C-033's explicit strip makes it deliberate |
| `ocx shell state` in **every** reportable state (inert, corrupt ledger, over-cap, yielded) | **0** | Still a diagnostic, never a failure — the enumerated inertness reason is the payload, not an error |
| `ocx shell state` cannot read `$OCX_HOME` | **74** `IoError` | The only non-zero path for this command |
| Refused `[shell.consent]` table that only **grants**, **any** tier | **0** | A-40 — the table is stripped, every other section of that file still applies, and the reason is logged, recorded on the payload for `ocx about`, and emitted through the eval'd script (A-21). A hard error would take `[registries]`, `[mirrors]` and `[[trust.policy]]` down with one typo — fleet-wide, on a `required = false` managed tier, silently |
| Refused `[shell.consent]` table carrying a non-empty `namespaces.exclude`, **home / system / `--config`** tiers | **78** `ConfigError` | A-40's exception — `exclude` is the only key that **withdraws** another tier's grant, and it accumulates across tiers, so stripping it leaves that tier's `include` standing unopposed. Dropping a withdrawal **widens**; the file keeps the hard failure it had before the strip existed |
| Refused `[shell.consent]` table carrying a non-empty `namespaces.exclude`, **managed** tier | **no exit code** | Same refusal, fleet blast radius: the snapshot goes unapplied with one WARN rather than failing every `ocx` invocation on every host (`config/loader.rs`, `log::warn!` + benign-absent fold) |
| Malformed `OCX_CONSENT_NAMESPACES` on the env channel | **no exit code** | The whole contribution is discarded with one warning and the config tiers stand alone (D3 forbids breaking a prompt) |
| `ocx self setup --hook` / `--completion` write failure | **74** `IoError` | **82 `DirtyRcBlock` does not apply** — the `[shell]` write is not fenced |

**No new `ExitCode` variant is introduced by this design.** `quality-rust-exit_codes.md` requires these
named; they are named here so no work package invents one.

**The startup path emits no diagnostics at all.** `ocx self activate` at shell start emits env-setting
shell code and nothing else: no hint line, no summary, no over-cap abandonment line, no direnv/mise
yield line, no managed-strip reason. **Every one of those messages is deferred to the first
`--reconcile` run**, and the corollary that makes the deferral reliable is normative: **the first
prompt of every shell always reconciles**, because layer 2's mtime fast path has no recorded
fingerprint to compare against and "no record" counts as changed (C-019, C-044). The line still
arrives, one prompt later. Where the hook is disabled entirely, `ocx about` and `ocx shell state` are
the only channels — stated, not silently lost.

Why the deferral rather than a per-tool suppression: powerlevel10k treats **any** console output during
zsh initialisation as an error, disables instant prompt and warns on every subsequent shell start, and
pwsh's `$ErrorActionPreference = 'Stop'` (C-043) is the same class. Sniffing
`POWERLEVEL9K_INSTANT_PROMPT` fixes one consumer of an open-ended class; deferring by one prompt
removes the class.

**Every emitted diagnostic passes that arm's own value escaper and rides as a `printf` format
*argument*, never as the format string** — the POSIX arms emit `printf '%s\n' '<escaped>' >&2`, through
`Shell::emit_message` (C-046). Passing the message as the format string is a second defect the escaper
does not cover: a `%` in a project path would be consumed as a conversion spec. **Exit code is
unchanged by all of this: 0.**

**Extended by [A-38](./adr_shell_env_addenda.md)** — the 16 KiB cap (C-004) bounds only the ledger's own
contribution. The combined argv+envp size is an **OS boundary**: an `E2BIG` on `execve`, for the
interactive shell or for an `ocx run` child, is an ordinary spawn failure that degrades through the
existing `Command::spawn()` → `std::io::Error` → `Error::Io` (**74**) path, identical to any other.
**No size accounting, no pre-flight check, no warning** — an ocx-specific one would be a warning
nothing consumes.

---

#### C-052 — Documentation surfaces (mandatory, and two of them are constrained)
**ADR:** Migration and Rollout (Documentation surfaces) — **plus two discovery corrections.**

Mandatory surfaces, enumerated so the plan can own them:

1. `website/src/docs/reference/environment.md` — new vars (`__OCX_ENV_STATE`, `OCX_NO_HOOK`,
   `OCX_CONSENT_PATHS`, `OCX_CONSENT_NAMESPACES`; `OCX_SHELL` recorded as reserved-never-read).
   **Correction — this is a rewrite, not an append.** Lines 55–59 currently carry the `_OCX_APPLIED`
   section, which states *"this variable and the per-prompt shell hook that managed it (`ocx shell hook`)
   have been removed"* and points project activation at `ocx direnv`. That prose becomes **false** the
   moment the hook ships. Rewrite the section; do not append a contradicting one below it.
2. `website/src/docs/reference/command-line.md` — `self setup` / `self activate` flags **including the new
   `--completion` / `--no-completion` pair on `setup`**, and **the new `ocx shell state` command**.
   **Correction — a docs constraint that must not be broken.**
   `test/tests/test_doc_command_reference.py` hard-codes tombstone expectations for the `{#shell-hook}`
   section: it must contain `"REMOVED"` (`TOMBSTONE_ANCHORS`, line 62), must **not** contain a `**Usage**`
   block (line ~196), and must reference `_OCX_APPLIED` (line ~275). This design does **not** resurrect
   `ocx shell hook` — its only new command is `ocx shell state` — so **those tombstones stay valid and
   must stay intact**. Any rewrite of the surrounding prose must leave the `{#shell-hook}` section's
   three asserted properties untouched.
3. A **new shell-integration page**, carrying: Decision 9's direnv/mise coexistence rules,
   `ocx shell state`'s diagnostic role, and the `unset __OCX_ENV_STATE` repair gesture **with its cost**
   (priors destroyed; a new shell is the clean floor) — which is what makes `__OCX_ENV_STATE` a
   user-facing contract.
4. `website/src/docs/user-guide.md` — activation + consent.
5. `.claude/rules/subsystem-cli.md`, `.claude/rules/subsystem-cli-commands.md`.
6. `.claude/rules/subsystem-file-structure.md` — **two edits, not one**: the `state/projects/` layout row,
   **and** the "`state/` is **not walked** by `ocx clean`" bullet, which C-023 amends. Leaving it as
   written makes the contract a second source of truth. Also **name the two `projects` directories** so
   the collision is documented once (C-022).
7. `.claude/rules/arch-principles.md` — glossary + ADR index.
8. `handshake_toolchain_cli.md` §2 + §7a.
9. Regenerated config JSON schema — and note C-035: the regeneration is currently **unverified by CI**,
   which is why that contract exists.

**No migration prose anywhere.** Pre-1.0: breaks just break. The changelog entry **is the commit
subject**; `CHANGELOG.md` is generated by `git-cliff` and must never be hand-edited.

Breaking-change subjects this design owes:
- `feat(shell)!: reconcile the toolchain environment at every prompt`
- `feat(package)!: refuse reserved OCX_* env keys in package metadata`

---

## 2. UX Scenarios

Format: **action → expected outcome → error/edge cases**. `Tier` names the cheapest tier that can
prove it (Validation section of the ADR): **1** = no shell, **2** = execute the emitted snippet, no pty,
**3** = real pty.

### 2.1 Lifecycle — enter, leave, switch

**S-001 — Enter a consented project.**
`cd` into a project with a valid stamp (or a matching grant) at an interactive prompt.
→ At that prompt: global scope applied first, project scope second (C-018); project **path-kind**
entries resolve ahead of global ones, while project **list-kind** contributions land **behind**
global's — the kind appends, and last-wins consumer semantics are what make that project winning
(C-013); project constants override global ones; one summary line
(`ocx: +JAVA_HOME ~PATH (acme, lock a1b2c3)`); `__OCX_ENV_STATE` now carries both scopes with `priors`
captured **after** global's apply.
Edge: entering with the hook disabled → nothing applied, no line at all (S-014..S-016).
Edge: entering an unconsented project → S-011.
Tier 3 (the `cd` itself) over tier-2 coverage of the apply.

**S-002 — Leave a project.**
`cd` out of the project (to a directory under no project).
→ The **project section only** is reverted: project-recorded list elements removed (never a PATH string
restore), project constants restored from `priors` **under the `C == L.applied` guard**. The **global
section is untouched** — the user has not left the global scope. `__OCX_ENV_STATE` retains `global`,
drops `project`.
→ **`JAVA_HOME` is restored, not discarded.** This is the primary revert case and the one the superseded
rule (c) broke (C-017).
Edge: a constant the user overrode by hand mid-session (`C ≠ L.applied`) is **left alone**, not restored
(C-015 rule 2).
Edge: the project's PATH element also appears in the global scope's desired set → it stays, because
retirement is scoped to what D no longer wants (C-016).
Tier 3.

**S-003 — Switch projects.**
`cd` directly from project A to project B (siblings, no shared ancestor project).
→ **One pass**: revert A's section, apply B's. Not two prompts, not a pop-then-push. Resolved PATH after
the switch contains **no** element from A. Ledger's single `project` slot now names B (`key`, `dir`,
`applied`, `priors` all B's).
Edge: B is inert (no consent) → A is still fully reverted, B applies nothing, one hint line.
Edge: B is nested **inside** A (A's `ocx.toml` is an ancestor of B's) → still a **switch**, not a push:
`ConfigLoader::project_path` returns the **nearest** `ocx.toml` (C-018).
Tier 3.

**S-042 — First prompt of a new shell.**
Open a new interactive shell in a directory under no project.
→ Global scope applied. `__OCX_ENV_STATE` is unset before the prompt runs (`set -u` safe via
`${__OCX_ENV_STATE-}`), the absent ledger is planned against `Ledger::empty()`, **no repair line is
emitted**, and the debug log — not a warning — records the absence.
This is the contract that keeps C-006's absent/corrupt distinction observable: a corrupt carrier in the
same position **does** emit a line.
Tier 2 (the emitted snippet) + tier 3 (that the hook fired at all).

### 2.2 Freshness in a live shell

**S-004 — `ocx add --global <tool>` picked up at the next prompt in the SAME shell.**
Run `ocx add --global ripgrep:14`, press enter, then type `rg --version` at the next prompt.
→ `rg` resolves to the newly installed binary. No new shell, no re-source, no `eval`.
Mechanism: `$OCX_HOME/ocx.lock` moved ⇒ fingerprint changed ⇒ recompose (C-019, C-020). The wrapper
function is **not** involved — a prompt intervened.
Edge: the same command run in **another terminal** produces the identical outcome in this one at its next
prompt — it is literally the same event (C-020).
Tier 2 (headline acceptance criterion).

**S-005 — `ocx update` freshness.**
Run `ocx update` (or `ocx self update`), then at the next prompt invoke the tool.
→ The **new version** resolves. The old version's `…/packages/<old>/bin` is **absent** from PATH, not
merely behind the new one (S-010).
Mechanism: lock and/or binary version moved ⇒ fingerprint changed.
Tier 2.

**S-006 — Same-command-line freshness through the wrapper.**
Type `ocx update && cmake --version` as **one** command line.
→ `cmake` resolves to the new version, with **no prompt in between**. This is the only thing the wrapper
buys (C-045).
Edge: `command ocx update && cmake --version`, `\ocx update && …`, `/abs/path/ocx update && …`, the same
inside a script or Makefile → each **degrades to next-prompt correctness**, never breaks. None is a
supported-vs-unsupported distinction.
Tier 3 (by definition — no prompt to hook one tier down).

**S-007 — `ocx remove --global <tool>`: the binary is GONE, not shadowed.**
Run `ocx remove --global ripgrep`, then at the next prompt run `rg`.
→ `command -v rg` finds **nothing** from ocx. Assert the removed package's `bin` segment **count on PATH
is zero** — not "something else is in front of it".
This is what the retirement rule exists for (C-016) and the assertion an apply-only reconciler passes
**vacuously**. Fault-inject the retirement rule to see it red.
Tier 2.

**S-008 — Branch switch that deletes a tool from `ocx.lock`.**
`git checkout` a branch whose `ocx.lock` drops a tool. No ocx command is run at all.
→ At the next prompt the tool's element is **retired** from PATH. Recompose in place, live scope, D
changed (C-016 trigger 2).
Edge: the checkout preserves both mtime and byte size of every watch-set member → **not detected** until
something else in the watch set moves (C-019's named ceiling). Accepted; the escape is
`unset __OCX_ENV_STATE`.
Tier 2.

**S-009 — Branch switch that changes ONLY `[env]`, lock untouched.**
`git checkout` a branch whose `ocx.toml` `[env]` differs; `ocx.lock` is byte-identical.
→ The changed constant is applied at the next prompt. `[env]` is in the watch set precisely because it
applies on its own authority independently of the lock (C-019).
Edge: activation is still gated by the consent predicate — `[env]` does **not** get a free pass just
because it needs no lock (C-025).
Tier 2.

**S-010 — Digest change produces no duplicate PATH entry.**
Bump a tool's digest (a lock change), then recompose.
→ `…/packages/<old>/bin` **count on PATH is zero**. Not "the new one is in front".
Why this is its own scenario: the two are **different strings**, so `move_to_front` cannot dedupe them —
an additive apply passes an "is the new one in front" assertion while leaving the stale one on PATH for
the rest of the session, and any later foreign prepend can put it back in front.
**Named fault injection:** make the list repair additive; this assertion must go red.
Tier 2.

**S-032 — Mid-session global retirement while a project scope is live.**
Sit inside a project. In **another terminal**, run `ocx remove --global foo`. Return and press enter.
→ At this shell's next prompt the **global** element for `foo` is retired, **while the project scope
stays applied and untouched**. No wholesale teardown of the global section.
This is the case the old one-line scope-exit wording read as forbidding (C-018).
Tier 2.

**S-039 — PATH does not grow across many prompts.**
Press enter N times in one session, inside and outside a project.
→ The segment count of PATH is **constant**; the ledger is unchanged on no-op prompts; the same snippet
eval'd N times leaves PATH **byte-identical**. Reuse the shipped `_assert_activation` shape (bin dir
present *exactly once* after a double source).
Tier 3 for the prompt loop; tier 2 for the double-eval idempotency.

### 2.3 Consent

**S-011 — Fresh clone is inert.**
`git clone` a repo carrying `ocx.toml` + `ocx.lock` naming arbitrary registries, `cd` in.
→ **Zero env change.** Exactly **one hint line**, emitted by the **first `--reconcile` run**, never by
`ocx self activate` at shell start — the startup path emits no diagnostics at all, and the first prompt
of every shell always reconciles, so the line arrives one prompt later (C-051). No stamp is written. No
`ProjectConfig` deserialization happens (C-028) — only the CWD walk's `stat`s and the lock parse.
Edge — the vacuity case that must be tested: a clone with `[env] PATH = { type = "path", value = "bin" }`
and **no `ocx.lock` at all** must be inert. An empty source set never satisfies clause 2 (C-025).
Edge: `ocx.lock` present but unreadable or unparseable → identical outcome to absent.
Edge: the negative verdict is cached in the ledger, so the *next* prompt in the same clone is stat-only
and reads no config (C-042) — stat-only over the **nine-member** watch set, the ledger-recorded
config-tier paths and the consent stamp included, with the raw `OCX_CONSENT_*` values folded into `fp`
(C-019). That is what makes the cache expirable: adding the grant from another terminal, or exporting
`OCX_CONSENT_PATHS`, activates at the **next** prompt rather than at the next shell start.
Tier 2.

**S-012 — A grant activates and writes no stamp.**
Operator sets `[shell.consent] paths = ["/workspaces/acme-monorepo"]`; user `cd`s into that checkout for
the first time, then the lock gains `ghcr.io/evil/tool`.
→ Activation proceeds **with no prompt and no ceremony**, and **`state/projects/<key>/` does not
exist** — before, during or after. Nothing on the activation path writes a stamp (C-027).
→ After the lock gains the unconsented source the project **still activates**: clause 3 is
unconditional and re-evaluated every prompt, so a `paths` grant is deliberately drift-blind. Later
activations do **not** run clause 1 — there is no stamp for them to run against.
→ Assert the pair directly: `evaluate(P) == Activate(Grant::Path)` **and** `state/projects/<key>/` absent.
**Named red state, both arms required:** re-introduce an auto-stamp and the directory-absence assertion
fails; make clause 3 conditional on clause 1 and the `Activate` assertion fails.
Edge: revoking the grant is **immediately effective** — the next prompt is inert, because no stamp was
ever derived from it.
Edge: a `namespaces`-granted project is drift-sensitive without any stamp, because clause 2
re-quantifies over the **store-corroborated** source set every prompt (A-39) — a source leaving the
grant goes inert at the next prompt, and so does one the store stops corroborating.
Tier 2.

**S-013 — Source-set drift re-confirms — for a stamped project only.**
A project carrying a **real consent stamp** (written by one of the six commands in C-024 — never by a
grant, C-027) has its lock changed so its source set is no longer a **subset** of the stamped set.
→ Inert, with the reason naming **which source is new**.
**Scope, normative:** this scenario covers **hand-stamped** projects. A `paths`-granted project has no
stamp and does not reach clause 1 at all (S-012); a `namespaces`-granted project goes inert on drift
through clause 2's own quantifier, not through this predicate.
Canonical case to test: a **same-cardinality swap** `ghcr.io/acme → ghcr.io/evil`. Ordinary growth
*inside* already-stamped sources does **not** re-confirm.
Edge: normalization must be exercised here — port preserved (`localhost:5000` ≠ `localhost`), registry
lowercased, default registry spelled `ocx.sh/…`, path truncated to the first segment (C-026).
**Named fault injection:** build the source set from a re-derived physical address; this test must fail.
Tier 2 (tier 1 for the predicate itself).

**S-033 — `ocx clean` and consent stamps.**
Run `ocx clean` with (a) an `[env]`-only project that has a stamp but **no** `ocx.lock` and therefore no
ledger entry, and (b) a stamp whose `project_dir` no longer exists.
→ (a) **retained**; (b) **collected**.
Edge: a stamp directory that is itself a **symlink** → skipped, never followed into `remove_dir_all`.
Edge: an **indeterminate** probe of `project_dir` (I/O error) → **retained**.
Edge: a `.tmp-*` staging name → skipped.
The (a) case is the one the earlier ledger-derived sweep silently broke, forever, on a GC schedule.
Tier 2.

**S-035 — `[shell]` cannot come from a project.**
(a) Put `[shell]` in an `ocx.toml`. (b) Fold a project tier that carries `[shell.consent]` into `Config`.
→ (a) **hard parse error** — refused, help text names `config.toml`. (b) the project-tier contribution is
**explicitly stripped**; the merged config carries no project-sourced `shell` key.
Both must be tests, not prose (C-033). Neither may rely on `ProjectConfig`'s `deny_unknown_fields`, whose
own docstring says it is a typo detector.
Tier 1.

**S-036 — Managed `[shell.consent]` under an unpinned vs a pinned source.**
Publish a managed `config.toml` carrying `[shell.consent]`, once with `[managed] source` on a **tag** and
once **digest-pinned**.
→ Unpinned: `[shell.consent]` **absent** from the merged config, and the reason reaches the user through
the `printf … >&2`-inside-the-eval'd-script channel (**not** `log::warn!`, whose stderr the shims
discard) and is recorded where `ocx about` surfaces it. Pinned: **present**.
→ The reason line is emitted by the **first `--reconcile` run**, not at shell start: the startup path
emits no diagnostics at all, and the first prompt of every shell always reconciles, so it arrives one
prompt later through `Shell::emit_message` — escaped, and as a `printf` **argument** (C-051, C-046).
Both halves are required — this is the only thing between an unpinned managed payload and a PATH-front
activation grant (C-034).
Edge: `[shell] hook` merges in **both** directions regardless of pinning, including forcing `true` over a
user's explicit `false` — over every **discovered** tier. A `--config` / `OCX_CONFIG` file still beats
the managed tier in both keys, and the digest gate does not apply to `[shell.consent]` arriving through
it at all (C-034).
Tier 2 (tier 1 for the merge itself).

**S-037 — `OCX_CONSENT_NAMESPACES` empty tokens grant nothing.**
Set `OCX_CONSENT_NAMESPACES='ocx.sh/acme/*,'`, then `'a,,b'`, then `','`, then `''`.
→ Each yields **exactly its non-empty patterns and nothing else**. Assert with an **untrusted** source
(`ghcr.io/evil/tool`) **not** matching in every case. No error is raised for any of them — an unset var
and an empty one are the same situation, and D3 forbids breaking a prompt over either.
**Named fault injection (this is what makes the assertion non-vacuous):** run the same values through a
parser that keeps empty tokens; `ghcr.io/evil/tool` **must** start matching. If it does not, the test is
not discriminating.
Same pair for `OCX_CONSENT_PATHS`: an empty token must not become a `PathBuf` that matches any project
directory.
Edge: a **non-empty malformed** pattern (`ocx.sh/acme-corp*`, bare `*`) → the **whole**
`OCX_CONSENT_NAMESPACES` contribution is discarded with one warning; the config tiers stand alone.
Tier 1.

**S-043 — Grammar boundaries at parse.**
Write, in a `config.toml` tier: `namespaces = "ocx.sh/acme-corp*"`, then `"*"`, then `""`, then
`{ include = [], exclude = ["x"] }`.
→ Each **fails `[shell.consent]` deserialization**, so that file fails to load and the tier contributes
nothing. **Never** read as a catch-all.
→ `namespaces = "ocx.sh/acme/*"` matches the source `ocx.sh/acme` and **nothing else** — identically to
the bare `"ocx.sh/acme"`, because a source is exactly two components. It does **not** match
`ocx.sh/acme-evil`. A three-component pattern (`ocx.sh/acme/team`) names a repository, not a source, and
is **rejected at parse** alongside the four forms above (A-27).
→ `paths = ["/home/u/project"]` does **not** match `/home/u/project-evil` (exact-directory, no prefix, no
glob), and does **not** match a symlinked checkout resolving to it (entries compared literally; the
project side canonicalized) — inert is the fail-safe direction.
→ Carve-outs are at **source granularity**, so a carve-out withdraws one organisation another tier's
`include` already covers, never a repository from an org: `{ include = ["ocx.sh/acme",
"ocx.sh/acme-compromised"], exclude = ["ocx.sh/acme-compromised"] }` consents to `ocx.sh/acme` and
refuses `ocx.sh/acme-compromised`, **regardless of which tier contributed either**. There is no
whole-registry spelling to carve out of — `ocx.sh/*` and a bare `ocx.sh` are both refused at parse
(A-27). The repository-granularity spelling
`{ include = ["ocx.sh/acme/*"], exclude = ["ocx.sh/acme/compromised"] }` is **rejected at parse** — the
`exclude` pattern has three components (A-27).
Tier 1.

### 2.4 Enablement

**S-014 — Hook disabled by `--no-hook` (rung 1).**
Shell start where the shim passes `--no-hook` (or a direct `ocx self activate --no-hook`).
→ No hook is emitted at all. PATH prepend, completions and the global env eval are unaffected.
`ocx shell state` names the rung as `--no-hook`.
Tier 2.

**S-015 — Hook disabled by `OCX_NO_HOOK` (rung 3).**
`export OCX_NO_HOOK=1`, start a new shell.
→ No hook emitted. `ocx shell state` names the rung as `OCX_NO_HOOK`.
Edge — the one that must be documented, not discovered: exporting `OCX_NO_HOOK=1` **mid-session** takes
effect at the **next shell start**, not the next prompt. `--reconcile` bypasses `Hook::enabled` entirely
(C-041). The already-running shell keeps the env it applied and reverts nothing.
Edge: `OCX_NO_HOOK=maybe` → **warns and falls back to the default** (`BooleanString` contract), it is not
truthy and not an error.
Tier 2.

**S-016 — Hook disabled by `[shell] hook = false` (rung 4), including the managed tier winning.**
(a) `ocx self setup --no-hook` writes `[shell] hook = false` into `$OCX_HOME/config.toml` (home tier,
surgical `toml_edit` edit, comments and unknown keys preserved). (b) A managed payload publishes
`[shell] hook = true`.
→ (a) new shells get no hook. (b) the managed tier **beats every discovered tier** (system → user →
home), so it wins over the home-tier write, the hook returns, and `ocx shell state` says so — naming
the rung **and the deciding tier by name**, never a hard-coded "managed".
Edge: a `--config` / `OCX_CONFIG` file setting `hook = false` **beats the managed `true`** — the
explicit tiers merge on top of the managed fold (`config/loader.rs:180-182`, C-034).
Edge: `ocx self setup` with **neither** flag writes **nothing** to `config.toml`.
Edge: `--config` / `OCX_CONFIG` never redirects this write.
Edge: a higher tier already setting the key → the write still lands and the CLI says which tier will win.
Edge: a missing `$OCX_HOME/config.toml` is created carrying only the one section.
Tier 2 (tier 1 for the ladder itself).

**S-041 — Reversal levels 1 and 2 are not independent.**
A fleet publishes `[shell] hook = false`; a user wants the hook on.
→ Two of the per-user levers (`[shell] hook = true` in a **discovered** tier, `ocx self setup --hook`,
which writes the home tier) write a config key the managed tier merges **over**, so neither escapes a
fleet policy. **Two escapes survive**, and both must be documented: `OCX_NO_HOOK`, which only escapes
in the *off* direction; and the **explicit tier** `--config` / `OCX_CONFIG`, which merges on top of the
managed fold and therefore escapes in **both** directions (`config/loader.rs:180-182`, C-034).
`OCX_NO_CONFIG=1` does not prune the explicit tier; only `OCX_NO_HOOK=1` makes a shell wholly inert.
Document these as the two that work; do not present four orthogonal levels.
Tier 1.

### 2.5 Coexistence

**S-017 — direnv is live for this project → yield the project scope.**
`DIRENV_DIR` is set and names the resolved project's canonical directory.
→ ocx applies the **global** scope only, **reverts any project scope it had already applied**, and prints
**one info line** naming direnv as the owner — from the **first `--reconcile` run**, never from
`ocx self activate` at shell start (C-051). `ocx shell state` reports `yielded to direnv` naming
`DIRENV_DIR` and the directory it names.
Tier 2.

**S-018 — mise is live → same treatment.**
`MISE_SHELL` or `__MISE_ORIG_PATH` present.
→ Identical to S-017, naming mise and the observed variable, and likewise deferred to the first
`--reconcile` run.
→ **Both sentinels set and matching**: the two checks are **independent `if`s, never an `elif` chain**
(C-049), so ocx yields on either alone and prints **one line per observed tool** — two lines here. Red
state: an `elif` between the checks silently suppresses the second tool's line.
Tier 2.

**S-019 — `.envrc` present but direnv NOT active → no yield.**
A repo carries `.envrc` (or `mise.toml`, or `.tool-versions`); the tool is not installed / not hooked /
not active in this shell.
→ ocx activates **normally**. A config file is evidence of someone else's workflow, not of a live hook.
Yielding here would leave the project **silently managed by nobody**.
Tier 2. This is the scenario that makes C-049's "live session state, never a file on disk" testable.

**S-020 — `DIRENV_DIR` names a DIFFERENT directory → no yield.**
direnv is active for some ancestor, not for this project.
→ Treated as absent; ocx proceeds normally.
Tier 2.

### 2.6 Degradation and repair

**S-021 — `unset __OCX_ENV_STATE` repair gesture.**
Inside a project with constants applied, run `unset __OCX_ENV_STATE`, press enter.
→ Next prompt sees an **absent** ledger: D is rebuilt from truth, lists are repaired **subtractively**
(every prefix-owned element not in D is removed), constants are **left in place** (never guess-unset).
The fingerprint is recomputed because it lived inside the ledger.
→ **`priors` are gone.** Leaving the project later will **not** restore `JAVA_HOME`; it keeps the
project's value for the rest of that shell's life. A **new shell is the clean floor**.
→ The gesture is **silent** — indistinguishable at the prompt from the ordinary first-prompt absence.
`ocx shell state` is what confirms it (S-022, reason 6, `first_prompt: false`).
Tier 2.

**S-027 — Ledger over cap.**
Construct a project whose encoded ledger exceeds 16 KiB.
→ `__OCX_ENV_STATE` **is still set**, to a **decodable marker-only ledger** — `v`, `fp`, `verdict` and
`over_cap` naming the abandoned scope, with both scope payloads dropped. No partial payload, no dropped
`priors` rung, no dropped `applied` rung: one rule, not a ladder (C-004). The variable is omitted only
if even the marker fails to encode.
→ The named scope is reconciled **exactly as an absent scope**: D rebuilt from truth, lists repaired
subtractively, constants left in place.
→ **One summary line** names the abandoned scope — **once per transition into the over-cap state**, not
once per prompt, and emitted by the first `--reconcile` run (C-051).
→ `ocx shell state` reports this as its own distinct reason, read **from the marker** — the one
degradation that **loses** information rather than repairing it.
**Named red state:** a build that omits the variable loses `fp` with it, so every later prompt
recomposes, re-overflows and re-reports; assert **zero** recomposes over five further prompts with a
static watch set.
Tier 2.

**S-028 — Corrupt, truncated, or unknown-tag ledger.**
Set `__OCX_ENV_STATE` to each of: `""`(no — that is unset), `"1"`, `"1."`, `".abc"`, `"1.<garbage>"`,
`"1.<truncated valid payload>"`, `"2.<valid payload for encoder 1>"`, `"x.abc"`, a payload with
`"v": 99`.
→ Every one is treated as **absent**: D rebuilt from truth, lists repaired subtractively, constants left
in place, **exactly one** line emitted (because a scope *was* applied and its record is gone — unlike
S-042), debug-level log, exit 0. **Never** a hard refuse; never a broken prompt; never a panic.
→ The `"2.…"` case is the forward-compat proof: an older binary **repairs rather than misreads**, which
is what lets encoder `2` ship with no migration.
Tier 1 for decode, tier 2 for the emitted consequence.

**S-029 — Binary removed mid-session (probe guard).**
Delete or un-execute the resolved ocx binary while a hooked shell is open; press enter.
→ The hook is a **silent no-op**. Nothing on stdout, nothing on stderr, exit 0, prompt renders normally.
Tier 2.

**S-030 — Rollback to a binary that rejects `--reconcile`.**
Point `current` at a pre-hook ocx (a rollback or a downgrade — **not** `OCX_BINARY_PIN`, which no shim
body reads and which has no effect on the `--reconcile` call, C-041); press enter in an already-hooked
shell.
→ **No output on either stream.** The emitted body discards the reconcile call's stderr and ignores its
exit status, so the clap unknown-flag usage error is invisible rather than printed **once per prompt in
every open terminal**.
Tier 2.

**S-025 — `set -u` safety.**
Run the hook under `set -u` (bash/zsh/ksh/dash) on the **first** prompt, where `__OCX_ENV_STATE` is unset
by construction.
→ No unbound-variable error. Every ledger read uses default expansion (`${__OCX_ENV_STATE-}` and per-shell
equivalents).
Tier 2, across the whole POSIX arm set.

### 2.7 Inheritance and containment

**S-023 — Subshell inheritance and containment.**
From a hooked shell inside project A, spawn a subshell; in the subshell `cd` to project B; exit back.
→ The subshell **inherits** the carrier and the env it describes, **atomically**. The subshell rewrites
the carrier **in its own environment only**; the parent's view is **intact** afterwards.
This is the property that makes the env carrier beat the on-disk alternative, and nothing else tests it.
Tier 2.

**S-024 — Cross-shell inheritance.**
A ledger written by one shell type is interpreted by a different one: `bash -c` under zsh, `fish -c` from
bash, `pwsh -Command` from either.
→ The child **decodes, plans and emits correctly in its own syntax**, from the same raw values, through
**its own** escaper.
This is Invariant L-2's end-to-end proof (C-009). A pre-escaped value leaking into the ledger would be
double-escaped by the inheriting shell and correctly escaped by none — silent and per-value.
No pty needed: spawn the child with the carrier inherited and assert its decode, plan and emitted result.
Tier 2.

### 2.8 Values, separators, escaping

**S-026 — A PATH element with hostile characters.**
A project `[env]` contributes a PATH element `/tmp/a';id;'b`, and constants carrying `'`, `"`, `` ` ``,
`$`, `\`, `%VAR%` and a newline.
→ **No execution, in any arm.** Each arm uses **its own** escaper: `escape_posix_single_quoted` for
bash/zsh/ash/ksh/dash, `escape_single_quoted_doubled` for PowerShell/elvish, `escape_value` for
fish/nushell only. Assert on the **emitted string**.
→ **Named fault injection:** route one arm through `escape_value` and watch the `'`-injection fixture go
red. `escape_value` is the **double-quoted**-context escaper and deliberately leaves `'` untouched, so
that arm would execute `id` at **every prompt**.
→ The same value round-trips through `encode`/`decode` **byte-identically** (C-009) — the ledger holds
raw text, never shell text.
Tier 1, and **nowhere else**: escaping is the one property whose tier-2/tier-3 failure would be a
*silent wrong value* rather than a visible one.

**S-034 — A non-default-separator list var applies AND reverts.**
`CFLAGS` declared `{type = "list", separator = " "}`; `CLASSPATH` declared `{type = "list", separator = ":"}`
on Windows.
→ Both apply **to the back** — `utility::list::append_unique` in process, `Shell::export_list` emitted,
the whole opaque contribution, never split into elements (C-013) — **and revert**, by flank-delimited
removal of that whole contribution, `remove_list_element` being `append_unique`'s inverse and never a
segment op. Assert the reverted value is byte-identical to the pre-apply value with foreign elements
preserved.
→ The separator passed on the revert is the **effective** one recorded in the ledger — always `Some`
for `kind == List`, defaulting to `" "` where nobody declared one (C-001). Assert `CFLAGS` reverts with
`" "` flanks: a `None`-preserving build emits `:` flanks and the contribution becomes permanently
unremovable.
Edge: without the separator parameter on `remove_list_element` the revert either removes nothing or splits
on the wrong byte and **corrupts the value** — the defect C-014's signature exists to prevent.
Tier 2 (the defect only surfaces once a real shell has split the value).

**S-031 — A user's mid-session override is neither clobbered nor laundered.**
Inside a project, `export JAVA_HOME=/my/jdk` by hand. Then (a) press enter several times; (b) edit the
project's `[env]` so `JAVA_HOME`'s composed value genuinely changes; (c) `cd` out.
→ (a) The override **survives every prompt** — apply is gated on `D ≠ L`, and a same-project prompt with
an unchanged fingerprint runs no apply at all.
→ (b) The project's **new intent wins inside the project** (that is by design), **and the prior is
re-captured** (`L.prior := C`), plus one summary line.
→ (c) On the way out, ocx restores `/my/jdk` — **not** an unset. Without prior re-capture, `C == L` would
hold again and ocx would **delete a variable the user set by hand**, which is strictly worse than direnv
restoring a stale one.
Edge (coincidence): if the user's typed value happens to equal D, ocx claims it silently with
`prior := C`; leaving then restores D rather than removing it. Accepted, documented leak.
Tier 2.

### 2.9 Shell-specific and diagnostic

**S-040 — nushell takes the structured-data path and lags one hop.**
Apply the reconciler on nushell.
→ `ocx self activate --reconcile --format json` returns the `Plan`; the inlined nu body applies it via
`load-env`. nushell never receives shell text.
→ **Not yet reachable as written** (C-048): both shipped nu paths — the `ENV_NU` startup body and the
PWD hook it appends — still call `ocx --format json --global env`, so this scenario's first line is
WP-12b's acceptance criterion, not a description of today. Until it lands, the nushell rows assert the
global-toolchain apply only, and the suite's `reconcile`-count probe skips the project-scope rows by
reading the shipped `env.nu` (see the Wave status note).
→ **`restores: (key, None)` (constant unset) is unimplementable** until the `hide-env` spike lands.
→ Every reconciler change reaches nushell **one `self update` hop** behind, because for nushell the
activation body **is** the shim body. Every other shell is immediate at the next shell start.
Edge: the hook must actually **fire** on `env_change.PWD` — the
[nushell#14944](https://github.com/nushell/nushell/issues/14944) class, where it silently did not fire on
a lowercase `PWD`. No non-pty test can distinguish a hook that never ran from one that ran and decided
nothing.
Tier 2 for the plan application and the spike; **tier 3 for "it fires at all"**.

**S-038 — Reserved keys, both directions.**
(a) `ocx package create` on metadata declaring `env` key `OCX_CONSENT_NAMESPACES` (or `__OCX_ENV_STATE`,
or `OCX_NO_HOOK`).
(b) Install an already-published package that carries such a key, then `ocx run`, `ocx exec`,
`ocx launcher exec`, and a shell compose.
→ (a) **Refused, exit 65.**
→ (b) **Keeps resolving.** The key is **skipped with a warning, once per compose**, on **every** path —
including the non-interactive ones automation actually uses, because the gate is at `Env::apply_entries`,
not at the shell emitter.
→ The attack this closes: a publisher inside one already-consented namespace shipping
`OCX_CONSENT_NAMESPACES = "*/*"`, which would compose into the shell at the next prompt, be inherited by
every child process, and silently convert the whitelist into allow-all.
Edge: `__OCX_ENV_STATE` is **not** stripped by `Env::apply_ocx_config` from a child env — `ocx run -- bash`
must hand the nested shell a consistent ledger.
Tier 1 for both halves.

**S-022 — `ocx shell state` on each inertness reason.**
Run `ocx shell state` in each of six states: (1) no stamp + no grant; (2) stamp present, source-set drift;
(3) hook disabled — once per rung, and once with the managed tier winning; (4) yielded to direnv, and to
mise; (5) ledger over cap; (6) ledger absent (first prompt) **and** ledger corrupt.
→ Each prints the **specific** reason, with its evidence: the derived source set and the grants it was
tested against; the source that is new; the deciding rung **and the deciding tier by name** (never a
hard-coded "managed", and the explicit `--config` / `OCX_CONFIG` tier is a possible answer); the live
signal observed, **one line per observed tool** when both direnv and mise are live; the abandoned
scope, read from the ledger's `over_cap` **marker** — not inferred from a missing carrier, which is a
different state (C-004); and — for (6) — **which of the two** situations it is.
→ Four further reason rows must each be individually reachable and individually tested: a **skipped
symlinked `ocx.toml` candidate** with the ancestor project activated instead, naming `--project` /
`OCX_PROJECT` as the opt-in (the loader's warn never reaches the prompt, so this row is the only path
to that answer); *"active via `paths` grant; source-set drift is not tracked for path grants"*; a
**`paths` near-miss** differing from the canonical directory only by ASCII case or separator style; and
the deciding config tier by name.
→ Also prints, always: the decoded ledger as **fields, not base64**; applied-per-scope with `global` and
`project` separate; fingerprint status per watch-set member; and **`priors` intactness** per constant the
project scope owns.
→ **Writes nothing**: no stamp, no ledger repair, no plan. Exit 0 in every state.
→ **Never eval-able**: for **every** one of the six reasons, no output line is valid `export` / `set` /
`$env.` syntax in **any** arm, and the output is **not interchangeable** with `self activate`'s stream.
Values are quoted-for-humans, never quoted-for-a-shell.
→ `--format json` is the root/context flag, not a subcommand surface, and carries the same content.
Tier 1 for the eval-ability and interchangeability assertions (two strings differing in the right way
needs no shell); tier 2 to reach each reason.

**S-044 — Prompt-hook coexistence with other prompt owners.**
Start bash under **starship**, zsh under **oh-my-zsh**, zsh under **powerlevel10k**; also bash with
`PROMPT_COMMAND` in its **string** form and in its Bash 5.1 **array** form.
→ ocx's hook is **appended, never clobbering**, in every case; the other tool's prompt still renders; and
**`$?` is preserved across the ocx hook** (the
[vscode#158090](https://github.com/microsoft/vscode/issues/158090) class).
Edge: zsh must use `add-zsh-hook precmd` / `precmd_functions+=`, **never** define `precmd()`.
Edge: PowerShell must **wrap** `prompt`, calling through to the captured previous definition.
Edge: nushell must **append** to `$env.config.hooks.env_change.PWD`, never assign it.
Tier 3.

**S-045 — Windows PowerShell 5.1.**
Run the full apply/revert/switch cycle on **PS 5.1**, on a Windows runner.
→ Full interactive fidelity via prompt-wrap. The only gap is a programmatic `Set-Location` that never
returns to a prompt — the non-interactive scripting path this hook does not serve.
→ `$env:PATH` vs `$env:Path` casing: the **same** variable on Windows, **different** variables on
Linux/macOS, so the platform-conditional casing arm is **untestable off-platform**.
→ Segment-exact matching: removing `C:\WINDOWS` must **not** also strip `C:\WINDOWS\system32`.
Tier 3, **on a Windows runner leg**. Tiers 1 and 2 run there unchanged. The existing
`test_shell_activation.py` skips the whole module on `win32`; that skip is that file's, not a precedent
the new suite inherits.

---

## 3. Traceability — ADR Decision → C-IDs → S-IDs

| Decision | Subject | C-IDs | S-IDs |
|---|---|---|---|
| **1** | Private state carrier `__OCX_ENV_STATE` | C-001, C-002, C-003, C-004, C-005, C-006, C-007, C-008, C-009, C-010, C-011, C-012, C-036, C-037 | S-021, S-023, S-024, S-026, S-027, S-028, S-038, S-042 |
| **2** | One project key, one per-project state root | C-022, C-023 | S-033 |
| **3** | Reconciler: typed three-way, provenance-tagged | C-010, C-011, C-013, C-014, C-015, C-016, C-017, C-018, C-019, C-020, C-021 | S-001, S-002, S-003, S-004, S-005, S-007, S-008, S-009, S-010, S-031, S-032, S-034, S-039 |
| **4** | Consent and the activation whitelist | C-024, C-025, C-026, C-027, C-028, C-029, C-030, C-031, C-032, C-033 | S-011, S-012, S-013, S-035, S-037, S-043 |
| **5** | Enablement, symmetric with completions | C-038, C-039, C-040, C-041, C-042, C-043, C-044, C-045, C-046 | S-006, S-014, S-015, S-016, S-025, S-029, S-030, S-041, S-044 |
| **6** | Regeneration: thin-dispatcher invariant, where lag lives | C-047, C-048 | S-040 |
| **7** *(= OD-2)* | `[shell]` in the managed tier | C-029, C-032, C-034 | S-016, S-036 |
| **8** *(= OD-3)* | Accept the silent digest swap; name the real mitigation | C-026 (residual + the `lock`/`update` seam) | **No scenario.** See note below. |
| **9** | Coexistence with other per-prompt PATH tools | C-049 | S-017, S-018, S-019, S-020 |
| **10** | `ocx shell state` | C-050 | S-022 |
| — | Exit codes and error semantics | C-051 | S-011, S-030, S-038 |
| — | Documentation surfaces + docs constraints | C-052 | — (doc surface; enforced by `test_doc_command_reference.py`) |
| — | Config schema generation is unverified by CI | C-035 | — (test contract, no user-facing scenario) |

**Decisions with no natural contract, stated explicitly:**

- **Decision 8 / OD-3** has **no behavioural contract**, and that is the decision: within a consented
  namespace, whoever can publish gets PATH-front code with **no signal**. It maps to **(a)** the residual
  clause inside **C-026**, **(b)** a **documentation surface** (C-052 — the new shell-integration page and
  the user guide must state it), and **(c)** an **accepted residual**. What it must **not** map to is a
  digest-set re-confirm: that fires on every legitimate lock bump (`git pull`, `ocx update`), training
  users to confirm without reading — a net security regression, and the reason every surveyed default-mode
  whitelist is static. mise's paranoid mode is the precedent for an *opt-in* content check, not a default.
  **The mitigation must be documented as default-off.** With no trust policy configured, auto-verify is a
  **no-op**; with no *matching* policy it logs INFO and the install **proceeds**; and it is not on the
  hook's path at all, since the hook is compose-only. Worse, absent an operator policy the project tier
  applies, so a repo-supplied `[[trust.policy]]` makes verification attest **the repo author's own
  identity** — a pass that proves nothing to the victim. The enabling step is an **operator-tier
  `[[trust.policy]]` in `config.toml`**, never `ocx.toml`. Documenting a default-off control as the
  residual's mitigation without saying it is default-off is the same class of error as importing another
  tool's benchmark as a budget.
- **Decision 6's lag table** rows (a) and (c) map to **no contract and no scenario**: (a) is a property of
  body regeneration already governed by the shipped `refresh_shims` / `refresh_profiles` diff-gate, and
  (c) — an already-running shell keeping its old activation — is **universal and unsolved by every
  surveyed tool**. Row (b) is the only one that needs work-package budget, and it is C-048 / S-040.
- **OQ-1a (strict AND mode)** and **`ocx shell refresh`** map to **recorded product decisions**, not
  contracts. Both are captured inside C-025 and C-050 respectively so they are not re-proposed.
- **NFR Latency** is not a component contract; it is a **gate with a named red state**, contracted inside
  C-044 (`exec_floor + Δ`, a single `Δ ≤ 2 ms` for both shell startup and the per-prompt reconcile —
  the 2026-08-25 amendment's `Δ ≤ 25 ms` split was a misdiagnosis, corrected the same day once the real
  cost was traced to `HostCapabilities::detect_and_cache` rather than the reconciler — floor measured in
  the same job, each red produced by a fault injection at a seam only that measured path reaches).

---

## 4. Recommended work-package cut

> ⛔ **VOID — SUPERSEDED IN FULL by [`plan_shell_env_overhaul.md`](./plan_shell_env_overhaul.md) §7.**
> The plan owns **decomposition, file ownership and sequencing**: its §7.1 (wave 0), §7.2 (work
> packages), §7.3 (conflict-file ownership), §7.4 (dependency graph), §7.5 (critical path and merge
> plan) and §7.6 (fault injections) are the only authority. Nothing below is executable — the WP
> numbering, the wave assignment, the file-ownership table and the DAG here are all superseded, not
> merely amended, and where the two disagree there is no contest. This section is retained as the
> **reasoning trail** behind that cut, so a reviewer can see why the boundaries fell where they did.
> **Do not implement from this section, and do not cite a WP number from it.**

The plan runs these as parallel git worktrees, so **file disjointness is a hard requirement**. The cut
below is driven by that constraint first and by subsystem coherence second.

### 4.0 Wave 0 — contract stubs (SEQUENTIAL, lands before any fan-out)

**This is not optional and it is not a work package.** It is the single sequential commit that dissolves
almost every file conflict in the parallel set, by owning **every module declaration, type shell and flag
struct** exactly once. Every WP below then compiles against a fixed API from its first commit.

Owns (edits, then never touched again by wave 0):

| File | Wave-0 edit |
|---|---|
| `crates/ocx_lib/src/shell.rs` | `pub mod reconcile; pub mod hook; pub mod coexistence;` + `remove_list_element` signature with `unimplemented!()` |
| `crates/ocx_lib/src/config.rs` | `pub mod shell;` + `pub shell: Option<ShellConfig>` field + `merge` arms with `unimplemented!()` |
| `crates/ocx_lib/src/project.rs` | `pub mod consent;` (+ the dead-code re-export deletion below) |
| `crates/ocx_lib/src/package/metadata/env/modifier.rs` | add `Deserialize` to `ModifierKind` (one derive) |
| `crates/ocx_lib/src/oci/identifier.rs` | the first-path-segment accessor (C-026) — **new code, nothing existing returns this** |
| `crates/ocx_cli/src/options.rs` | `pub mod hook;` |
| `crates/ocx_cli/src/command.rs`, `command/shell.rs` | the `ShellState` subcommand variant + dispatcher arm |
| `crates/ocx_cli/src/api/data.rs` | `pub mod shell_state;` |
| `crates/ocx_lib/src/setup.rs` | `pub mod shell_config;` |

Also in wave 0 — **delete the dead code** (see §4.3). Mechanical, no design content, and doing it here
stops any parallel builder from finding it mid-task.

**Gate:** `cargo check --workspace --all-targets` green with `unimplemented!()` bodies.

### 4.1 The cut — 14 work packages

| WP | Scope (one line) | Files it OWNS | Decisions |
|---|---|---|---|
| **WP-1** | Ledger types, envelope codec, degradation, forgery rules, `plan`, `Plan` + its JSON shape | `crates/ocx_lib/src/shell/reconcile.rs` and everything under `crates/ocx_lib/src/shell/reconcile/` | 1, 3 |
| **WP-2** | `Shell::remove_list_element` — 10 arms, per-arm escaper, Batch `None`, the five hazards; **plus** the in-process/emitted parity tests | `crates/ocx_lib/src/shell.rs` | 3 |
| **WP-3** | Per-shell hook + wrapper body emission, append-only registration, zero-exec short-circuit, `set -u`, `printf >&2` channel | `crates/ocx_lib/src/shell/hook.rs` | 5 |
| **WP-4** | direnv/mise live-session detection returning a typed `Yield` verdict | `crates/ocx_lib/src/shell/coexistence.rs` | 9 |
| **WP-5** | `StateStore` project-scoped accessors + the `ocx clean` sweep with its four guards | `crates/ocx_lib/src/file_structure/state_store.rs`, `crates/ocx_lib/src/package_manager/tasks/clean.rs` | 2 |
| **WP-6** | `ConsentStamp`, `record`, `evaluate`, source normalization, the write seam covering all six commands | `crates/ocx_lib/src/project/consent.rs`, **+ the located write seam** (see risk below) | 4 |
| **WP-7** | `ShellConfig`/`ShellConsent`, grammar-at-parse, env channel, `Config::merge`, managed digest-pin gate, project-tier strip, **schema test** | `crates/ocx_lib/src/config/shell.rs`, `crates/ocx_lib/src/config.rs`, `crates/ocx_lib/src/config/loader.rs`, `crates/ocx_schema/**` | 4, 5, 7 |
| **WP-8** | Reserved-key gate at the application seam + `package create` rejection at 65 | `crates/ocx_lib/src/env.rs`, `crates/ocx_lib/src/package/metadata/validation.rs` | 1 |
| **WP-9** | `options::Hook` + `Completion::enabled` gaining `configured`; both five-rung ladders | `crates/ocx_cli/src/options/hook.rs`, `crates/ocx_cli/src/options/completion.rs` | 5 |
| **WP-10** | `self setup --[no-]hook` / `--[no-]completion` + the **new** surgical `toml_edit` home-tier writer | `crates/ocx_cli/src/command/self_group/setup.rs`, `crates/ocx_lib/src/setup/shell_config.rs` | 5 |
| **WP-11** | `self activate`: `--hook`, hidden `--reconcile`, cross-version rules, probe guard, emission order, yield wiring, config read point | `crates/ocx_cli/src/command/self_group/activate.rs` | **5, 9, 10** |
| **WP-12** | Shims: nushell JSON-`Plan` apply body + the thin-dispatcher guard (ceiling + denylist) | `crates/ocx_lib/src/setup/shims.rs` | **5, 6** |
| **WP-13** | `ocx shell state` — read-only report, enumerated reasons, never-eval-able assertions | `crates/ocx_cli/src/command/shell_state.rs`, `crates/ocx_cli/src/api/data/shell_state.rs` | 10 |
| **WP-14** | Acceptance suite (tiers 2–3) + every documentation surface + the three rule files | `test/tests/test_shell_reconcile.py` (new), `website/src/docs/**`, `.claude/rules/subsystem-*.md`, `.claude/rules/arch-principles.md`, `.claude/artifacts/handshake_toolchain_cli.md` | all |

### 4.2 The four conflict files — explicit ownership

| Conflict file | Owner | Why this WP, and how the other Decisions reach it |
|---|---|---|
| `crates/ocx_cli/src/command/self_group/activate.rs` | **WP-11, sole owner** | Decisions 5, 9 and 10 all land here, and they land as *emission-order* decisions in one function — splitting them puts two agents inside `emit_activation`. WP-4 (yield detection) and WP-13 (`shell state`) provide **library seams** WP-11 calls; **neither touches this file.** That is also why C-049 puts the direnv/mise detection in `ocx_lib` rather than inline: it is what makes the split possible at all. |
| `crates/ocx_lib/src/config/loader.rs` | **WP-7, sole owner** | Decisions 4, 5 and 7 land here as **three edits inside one idiom** — `guard_managed_sigstore_trust`'s home hosts both the managed digest-pin gate (C-034) and the project-tier `[shell]` strip (C-033), and the `self activate` read point (C-042) only *calls* `load_with_local_view`, it does not edit it. One owner, one idiom, one reviewer. |
| `crates/ocx_lib/src/config.rs` | **WP-7, sole owner** | The `pub shell` field and the `mod shell` line land in **wave 0**; every subsequent edit (`merge` arms per C-032) is WP-7's. |
| `crates/ocx_lib/src/setup/shims.rs` | **WP-12, sole owner** | Decisions 5 and 6 do touch disjoint per-family consts — but the **thin-dispatcher guard (C-047) is one test over a per-family ceiling-constant table plus one shared denylist**, i.e. a single artifact spanning all five families. Splitting per-family would put two writers on that guard. Do **not** split this file. |

### 4.3 Dead code — recommendation: **delete, in wave 0**

Two orphans, verified zero call sites workspace-wide, both left behind when the `ocx shell hook` /
`shell direnv` commands were deleted:

| File / symbol | Evidence |
|---|---|
| `crates/ocx_lib/src/shell/applied_set.rs` — `AppliedEntry` | referenced only by `shell.rs`'s `mod` line and its own tests |
| `crates/ocx_lib/src/package_manager/tasks/hook.rs` — `AppliedSet`, `collect_applied` | re-exported at `crates/ocx_lib/src/package_manager.rs:287`, consumed nowhere |

Plus the one re-export line (`package_manager.rs:287`).

`crates/ocx_lib/src/project/hook.rs` was proposed for deletion and is **not** dead —
`load_project_state` / `ProjectState` / `MissingState` are called from
`crates/ocx_cli/src/command/direnv_export.rs:11,94,96,102`. It stays.

**They do not overlap the proposed ledger.** `AppliedEntry`/`AppliedSet` are the shape of the *deleted*
`_OCX_APPLIED` fingerprint mechanism, not of `__OCX_ENV_STATE`; nothing in C-001..C-011 derives from them.

**Delete, and do it in wave 0.** Three reasons, in order of weight:
1. **It is actively hazardous to leave them for this specific plan.** A builder implementing the ledger
   who greps `applied` or `hook` inside `ocx_lib` will find a plausible-looking `AppliedSet` /
   `collect_applied` pair and either extend it or model the new types on it. That is the most likely way
   this plan ships the wrong shape.
2. The repo's own doctrine: delete dead code; refactor as if the removed feature never existed; no compat
   shims, no orphan re-exports.
3. It is mechanical — two file deletions and one re-export line — so it belongs in the sequential stub
   wave, not inside a design-bearing WP where it would inflate a review diff.

### 4.4 Dependency DAG and what is forced sequential

```
wave 0 (stubs + dead-code deletion)   [SEQUENTIAL — blocks everything]
   ├── WP-1  ledger + planner ─────────┬──> WP-11 (activate)
   ├── WP-2  remove_list_element ──────┤     ^
   ├── WP-3  hook emission ────────────┤     |
   ├── WP-4  coexistence ──────────────┘     |
   ├── WP-9  options ────────────────────────┘
   ├── WP-5  state store + clean        (independent)
   ├── WP-6  consent ──────────────────┬──> WP-13 (shell state)
   ├── WP-7  config ───────────────────┘     ^
   ├── WP-8  reserved keys              (independent)
   ├── WP-10 self setup                 (independent after WP-9's stub)
   ├── WP-12 shims  [GATED on the hide-env spike; needs WP-1's Plan JSON]
   └── WP-13 shell state ──────────────────> (consumes WP-1, WP-4, WP-6 seams)
WP-14 tests + docs                      [two passes — see below]
```

**Forced sequential, and why:**

1. **Wave 0.** Unavoidable. Every mod line, every type shell, one writer. Without it, eight WPs collide on
   `shell.rs`, `config.rs`, `project.rs`, `options.rs` and `command.rs` on their first commit.
2. **WP-11 (`activate.rs`) integrates last among code WPs.** It is the composition point for Decisions 5,
   9 and 10; it can start on flag wiring immediately after wave 0, but its emission body is only
   meaningful once WP-1, WP-3, WP-4 and WP-9 have landed. Sequence it as a *late* worktree, not a
   *blocked* one.
3. **WP-12 is gated, not merely dependent.** C-048 requires a **red+green spike** proving element removal
   *and* unset on a real nushell (`hide-env` scoping inside hook blocks) **before any parity claim**.
   Schedule the spike as WP-12's first task with its own gate; if it reds, the nushell constant-revert
   ships as documented-unimplemented rather than as a claim. Do not let this gate block the other twelve.
4. **WP-14 runs in two passes.** Pass A (parallel, immediately): everything whose grammar the ADR already
   fixes — the new shell-integration page, `environment.md`'s **rewrite** of the `_OCX_APPLIED` section,
   the user guide, `subsystem-file-structure.md`'s two edits, `arch-principles.md`. Pass B (last,
   sequential): `command-line.md`'s `ocx shell state` and `self setup` flag rows, which must match WP-13's
   and WP-10's final `--help` text, and which must **not** disturb the `{#shell-hook}` tombstone
   assertions (C-052).
5. **The tier-2/tier-3 acceptance suite is WP-14's, not each WP's.** Per-WP unit tests (tier 1) stay with
   their WP; the shell matrix is one file with one owner, extending `test/tests/test_shell_activation.py`'s
   shape (`_POSIX_SHELLS` parametrization, `_script_pty_command`, `_clean_env`, shell-zoo image). Splitting
   the matrix across worktrees is how a shell suite becomes unmaintainable.

**Two risks the planner should price in:**

- **WP-6's write seam is not yet located.** C-024 requires a seam covering `add`/`remove`/`lock`/`update`/
  `pull`/`run`, and `register_project_dir_best_effort`'s two call sites (`project/lock.rs`,
  `project/mutation.rs`) cover **neither `run` nor `pull`**. The likely home is the shared project-resolve
  prologue in `crates/ocx_cli/src/app/project_context.rs`, which no other WP owns — but that must be
  **verified first, in WP-6's first hour**, because if the seam turns out to need per-command edits it
  collides with several CLI files at once and the WP has to be re-cut.
- **WP-2 and WP-1 are file-disjoint but semantically coupled** — the planner emits what the primitive
  renders, and C-021's parity tests span both. Wave 0's `remove_list_element` signature stub removes the
  compile-level block; assign C-021's parity tests to **WP-2** (it owns `shell.rs`, where the sibling
  `live_*` tests already live) and have WP-1 consume the primitive through the stubbed signature only.

---

## 5. Corrections register

Seven verified discovery findings that supersede the ADR's own wording. Each is already folded into the
contract named; this table exists so a reviewer can check the fold happened.

| # | Correction | Folded into |
|---|---|---|
| 1 | The home-tier `config.toml` write cited as `setup.rs:389` is in **`crates/ocx_lib/src/setup.rs`**, not the CLI crate — and the shipped `--managed` write is **not** a `toml_edit` edit. It reads the whole file as a string and drives a **fenced-block state machine** via `setup/rc_block.rs` (`toml::to_string` of the whole `[managed]` table inside a labeled fence, `Fresh`/`Current`/`FormatUpgraded`/`Dirty`, exit 82 on user edits). The shipped write shares only the **target path**; the `toml_edit` mechanism is **genuinely new**, and `[shell]` must **not** be fenced. | **C-040** (contract restated accordingly; exit 82 explicitly does not apply), **WP-10** (new file `setup/shell_config.rs`) |
| 2 | `website/src/public/schemas/` is **gitignored and generated** (`website/.gitignore:18-19`), not checked in. Config-schema generation is **not exercised by PR CI**: `verify-basic.yml` / `verify-deep.yml` run `task schema:generate`, which builds `metadata/v1.json` only, and **no test anywhere calls `schema_for("config")`**. A broken `ShellConfig` `JsonSchema` compiles clean and passes `task verify`. | **C-035** (new contract: a schema test with a demonstrated red state), **WP-7** |
| 3 | `Identifier` (`crates/ocx_lib/src/oci/identifier.rs`) has `registry()` :169, `repository()` :174, `name()` :182, `tag()` :191, `digest()` :201 — and **no first-path-segment accessor**. The source normalization needs **new code**. | **C-026**, **wave 0** (accessor stub), **WP-6** |
| 4 | `OCX_NO_COMPLETIONS` is read as a **bare string literal** via `env::flag("OCX_NO_COMPLETIONS", false)` in `crates/ocx_cli/src/options/completion.rs:44` — it is **not** in `ocx_lib`'s `env::keys`. `OCX_NO_HOOK` follows that precedent; do not add either to `env::keys`. | **C-038** |
| 5 | `Completion::enabled` has **exactly one call site**: `crates/ocx_cli/src/command/self_group/activate.rs:102`. The signature change is a one-line blast radius plus three unit tests. | **C-039**, **WP-9** |
| 6 | `crates/ocx_lib/src/shell/applied_set.rs` (`AppliedEntry`) and `crates/ocx_lib/src/package_manager/tasks/hook.rs` (`AppliedSet`, `collect_applied`, re-exported at `package_manager.rs:287`) are **dead code**: zero call sites workspace-wide, orphaned when `ocx shell hook` / `shell direnv` were deleted. They do **not** overlap the proposed ledger. **A third claim — that `crates/ocx_lib/src/project/hook.rs` was also dead — was wrong** and is withdrawn: a workspace grep that excluded the defining file itself missed `crates/ocx_cli/src/command/direnv_export.rs:11,94,96,102`, which calls `load_project_state` / `ProjectState` / `MissingState`; a full-workspace grep including call sites outside `ocx_lib` catches it. | **§4.3 — recommendation: delete the two genuine orphans, in wave 0; `project/hook.rs` stays**, with the reasoning stated there |
| 7 | `test/tests/test_doc_command_reference.py` hard-codes tombstone expectations for `{#shell-hook}` in `command-line.md`: it must contain `"REMOVED"` (line 62), must **not** contain a `**Usage**` block, and must reference `_OCX_APPLIED`. This design does **not** resurrect `ocx shell hook` (its only new command is `ocx shell state`), so the tombstones **stay valid** — but any rewrite of surrounding prose must leave them intact. Separately, `website/src/docs/reference/environment.md:55-59` states *"the per-prompt shell hook … has been removed"*; that prose becomes **false** and must be **rewritten, not appended to**. | **C-052**, **WP-14 pass B** |

---

## 6. Open items

Two open, one closed. Everything else the ADR left ambiguous is resolved above under `ASSUMPTION:`.

- ~~**[NEEDS CLARIFICATION: the consent write seam's home.]**~~ **CLOSED** by
  [`plan_shell_env_overhaul.md`](./plan_shell_env_overhaul.md) §9 Risk 1. The seam is
  `crates/ocx_cli/src/app/project_context.rs`: all six commands route through two sibling functions
  there, so the file is right — but the stamp is **per-caller opt-in, never a blanket call**, because
  `load_project_with_lock` has three further callers (`inspect.rs:147`, `patch_freeze.rs:81`,
  `toolchain_env.rs:336`) and a blanket stamp would auto-grant consent on `ocx inspect` and `ocx env`,
  widening a security control beyond its stated set, silently. The plan carries the test that the three
  non-members do not stamp; C-024's six-command allowlist and C-027's "nothing on the activation path
  writes a stamp" are the two halves this must satisfy.
- **[NEEDS CLARIFICATION: the per-family shim body-size ceilings (C-047).]** The guard needs a concrete
  byte (or line) number per family. Those numbers must be **measured from the shipped bodies plus
  headroom** — inventing them here would either make the guard vacuous or red on day one. WP-12 sets them
  and must demonstrate the red by inlining a denylisted token.
- **[NEEDS CLARIFICATION: whether nushell's `hide-env` scoping permits constant-revert inside a hook
  block.]** C-048 makes this an explicit spike gate rather than a design assumption; the answer decides
  whether nushell ships full parity or documented-partial parity. It cannot be settled from the ADR, the
  code, or reasoning — only from a red+green spike on a real nushell.
  **Blocking prerequisite of the spike — met (#349).** `run_script` still returns `None` for an absent
  interpreter, but a skip can no longer read as a pass: `assert_every_present_interpreter_ran` fails
  when an interpreter that IS installed ran nothing, and observes the cause of every skip rather than
  inferring it. `nu` and `elvish` are installed and named in `__OCX_TESTING_REQUIRE_LIVE_SHELLS` on
  the unit-test leg (`verify-basic.yml`) and on the Debian shell zoo (`test/taskfile.yml`), so their
  absence fails those jobs. A nu result now counts as evidence.
