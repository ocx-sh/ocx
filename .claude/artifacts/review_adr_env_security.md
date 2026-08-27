# Security Review — `adr_shell_env_overhaul.md`

- **Reviewer**: security pass (Phase 5), opus
- **Date**: 2026-08-24
- **Target**: `.claude/artifacts/adr_shell_env_overhaul.md` (350 lines)
- **Status**: COMPLETE
- **Method**: every ADR claim that cites code was re-verified by grep/read against the tree at `goat@7285d639`. Findings marked CONFIRMED are backed by the cited file:line; nothing here is PLAUSIBLE-only.

## Summary

| ID | Severity | Finding | Confidence |
|---|---|---|---|
| B-1 | **Block** | `remove_path_element`'s stated escaper (`escape_value`) is the wrong one for 7 of 10 shells and yields quote-breakout RCE in the eval'd prompt stream | CONFIRMED |
| B-2 | **Block** | The reserved-key read-path gate is specified at emit, not at `apply_entries`; the ADR's three new `OCX_*` vars are a package-reachable, persistent trust grant | CONFIRMED |
| B-3 | **Block** | `namespaces` has no enforced grammar, the only shipped prefix matcher is a substring glob, and the "no typosquat at the OCI level" rationale is false on a public registry | CONFIRMED |
| B-4 | **Block** | "A forged ledger's worst case is a wrong diff" is false — kind confusion, removal suppression and `key`-as-path are three concrete primitives | CONFIRMED |
| W-1 | Warn | `ConfigLoader` already wires an (unused) project tier into the struct that will carry `[shell.trust]`; the "structurally impossible" claim has an expiry date | CONFIRMED |
| W-2 | Warn | The digest-pin gate on `[shell.trust]` is the single guard, and its WARN lands on a stderr the shims discard | CONFIRMED |
| W-3 | Warn | OD-3's named mitigation is opt-in, off by default, not on the hook's path, and absent an operator policy is authored by the attacker | CONFIRMED |
| W-4 | Warn | `sources` granularity is never pinned; the new `state/projects/` sweep drops the guards Option C was chosen to preserve | CONFIRMED |
| S-1 | Suggest | Say which side of the `paths` comparison is canonicalized — the two choices have opposite failure modes | CONFIRMED |
| S-2 | Suggest | The 64-bit truncated key is the only project identity unless `evaluate` also compares the stamp's `project_dir` | CONFIRMED |
| S-3 | Suggest | Make "consent before **parse**" normative, not just "consent before apply" | CONFIRMED |

**Verdict**: the architecture is sound and the threat model is the right one — the four Block findings are all *specification* defects (a wrong escaper name, a gate at the wrong seam, an unenforced grammar, an over-generous claim), not design flaws. Each has a fix measured in sentences, and none of them requires revisiting a Decision.

## Findings

---

### B-1 — BLOCK — `remove_path_element`'s stated escaper is the wrong one, and yields shell injection

**CONFIRMED.**

- **ADR**: Component Contracts, `ocx_lib::shell::Shell` row (line 273) — *"Every arm routed through the existing `escape_value`"*.
- **Reality**: `escape_value` (`crates/ocx_lib/src/shell.rs:476-528`) escapes for a **double-quoted** context and deliberately does **not** escape `'`. The shipped `export_path` — the very idiom `remove_path_element` mirrors — routes **7 of 10** arms through two *different*, private escapers:
  - bash/zsh and ash/ksh/dash → `escape_posix_single_quoted` (`shell.rs:549-552`, `'` → `'\''`), emitted as `__ocx_p='<value>'`.
  - PowerShell and elvish → `escape_single_quoted_doubled` (`shell.rs:537-539`, `'` → `''`).
  - Only Fish, Nushell and Batch use `escape_value` (`shell.rs:199`, `:237`, `:259`) — and those three arms are the ones that emit into a **double-quoted** context, which is exactly what `escape_value` is for.
- **Attack**: an implementer who follows the ADR literally writes `__ocx_p='{escape_value(raw)}'` for the bash arm. A PATH element containing a single quote (`/tmp/a';id;'b`) closes the literal and executes. The value reaches the emitter from two attacker-controlled surfaces: project `[env]` values (literal strings straight out of `ocx.toml`, `crates/ocx_lib/src/project/env.rs`) and package-metadata env values (`Entry.key`/`Entry.value` are plain `String`, `crates/ocx_lib/src/package/metadata/env/entry.rs:11-12`).
- **Precondition**: a consented project, or any package in the global tier. No further access needed.
- **Impact**: arbitrary command execution inside the eval'd per-prompt stream — every prompt, in the user's interactive shell. This is the worst outcome the whole consent model exists to bound, reached by a spec wording defect rather than a design flaw.
- **Fix**: state the escaper **per arm** in the contract (or better: state the invariant "byte-identical emit and match with `export_path`, therefore the same escaper as that arm"). The ADR's own D3/`move_to_front` "byte-identical between in-process and emitted forms" language (line 125) already implies this — the `Shell` row contradicts it.
- **Note on the two hazards the ADR *does* name** — both are already solved in the shipped code and the solution must be carried, not re-derived: zsh glob over-match and bash `${//}` pattern escaping are closed by quoting the expansion **inside** the pattern (`${KEY//:"$__ocx_p":/:}`, `shell.rs:174-176`), which forces literal matching. The strict-POSIX arms sidestep it entirely by passing the value through `ENVIRON` to `awk` rather than `-v` (`shell.rs:186-190`).

### B-2 — BLOCK — the reserved-key read-path gate must sit at `apply_entries`, not at emit; the ADR's three new env vars are a package-reachable trust grant

**CONFIRMED.**

- **ADR**: Decision 1 → *"Spoof-channel closure"* (line 72) — *"compose/emit **skips such keys with a warning** (read path)"*. Framed entirely around `__OCX_ENV_STATE`.
- **Reality (grep, all call sites)**: `is_reserved_ocx_key` is enforced in exactly three places — `crates/ocx_cli/src/options/env_override.rs:170` (`ocx run --env`), `crates/ocx_lib/src/env.rs:1406` (`OCX_ENV` decode), `crates/ocx_lib/src/project/env.rs:158` (project/group `[env]`). **Zero** call sites in `package/metadata/env/**` or `package_manager/composer.rs`. The ADR's "package metadata env is ungated" claim is correct.
- **The undiscussed consequence**: `Env::apply_entries` (`crates/ocx_lib/src/env.rs:578-608`) applies **every** entry unfiltered — `ModifierKind::Constant => self.set(&entry.key, &entry.value)`. `Env::apply_ocx_config` (`env.rs:~430-512`) then asserts authority over an **enumerated** key list (`OCX_GLOBAL`, `OCX_CONFIG`, `OCX_PROJECT`, `OCX_INDEX`, `OCX_MIRRORS`, `OCX_PATCHES`, `OCX_PATCH_SNAPSHOT`, `OCX_MANAGED_CONFIG`, `OCX_NO_VERIFY`, `OCX_ENV`, `OCX_ALLOW_YANKED`) — it is **not** a namespace strip.
- **Attack**: this ADR introduces `OCX_TRUST_PATHS`, `OCX_TRUST_NAMESPACES` and `OCX_NO_HOOK`. None will be on that enumerated list. A publisher inside **one** already-consented namespace ships a package whose metadata declares `OCX_TRUST_NAMESPACES = "*/*"` (or `OCX_TRUST_PATHS` naming a directory they can seed). The value composes, lands in the user's shell env at the next prompt, is inherited by every child and every nested shell, and — because the env channel is documented as **additive, unioned with the config tiers** (ADR line 178) — silently converts the whole trust whitelist into "allow all". Consent escalates from one namespace to every future clone, persistently, with no signal.
- **Impact**: full defeat of Decision 4 from inside a single trusted namespace. Also the cheap denial variant: `OCX_NO_HOOK=1` from a package silently disables the reconciler in the victim's shell.
- **Fix**: the read-path skip must be specified at the **application seam**, `Env::apply_entries` / the resolver, not at `conventions.rs::emit_lines`. `emit_lines` is the shell-text path only; `ocx run`, `ocx exec` and `ocx launcher exec` all reach the child env through `apply_entries` and would keep the hole. Alternatively (stronger, and matching the existing idiom) extend `apply_ocx_config` from an enumerated list to a namespace strip. State this explicitly in the Component Contracts table, which today carries no row for it.

### B-3 — BLOCK — the `namespaces` grammar has no stated validation, and the "no typosquat at the OCI level" rationale is false

**CONFIRMED.**

- **ADR**: Decision 4, line 176 — *"`namespaces` is a namespace prefix with a single trailing `/*` … at the OCI level there is no filesystem to typosquat into — an attacker cannot create `ocx.sh/acme-corp-evil/…` inside an operator-controlled namespace."* Component Contracts (line 274) declares `ShellTrust { paths: Vec<PathBuf>, namespaces: Vec<String> }` with **no validation and no matcher named**.
- **Reality — the shipped matcher is a substring glob.** `crate::trust::pattern_matches` (`crates/ocx_lib/src/trust.rs:374-383`):
  ```rust
  match pattern.find('*') {
      Some(index) => target.starts_with(&pattern[..index]),
      None => target == pattern || target.starts_with(&format!("{pattern}/")),
  }
  ```
  Its own doc comment says so: *"a bare `ghcr.io/acme*` is an intentional substring glob."* So `ocx.sh/acme*` **does** match `ocx.sh/acme-evil/tool`. This is the one pre-existing prefix matcher in the tree and the one a type-economy-minded implementer will reuse.
- **The rationale is wrong on a public registry.** The sibling-typosquat vector is not removed by moving from filesystem paths to OCI namespaces — it is *relocated*, to a primitive that is cheaper than creating a sibling directory: registering the account/org `acme-corp-evil` on `ghcr.io`, `docker.io` or the default `ocx.sh`. "Operator-controlled namespace" governs `acme-corp`, never its siblings. The research artifact's threat-model item 1 says the mitigation works because *"there is no filesystem to typosquat into"*; that is true only for a registry where namespace creation is closed, which the ADR never requires.
- **Attack**: an operator (or a user) writes `namespaces = ["ocx.sh/acme-corp*"]` — the trailing slash is one keystroke and the ADR's own prose is the only thing that forbids it. Attacker registers `ocx.sh/acme-corp-evil`, publishes a tool, and any clone whose lock resolves against it activates silently, PATH-front, with no prompt.
- **Impact**: full bypass of Decision 4 for any whitelist entry missing the `/`.
- **Fix, two lines**: (1) make the grammar enforced, not documented — reject at parse/merge any `namespaces` entry containing `*` anywhere other than as a final `/*`, and reject a bare `*`; a rejected entry is dropped, not treated as a catch-all. (2) Do the matching on **segment boundaries**: strip the trailing `/*` and reuse `pattern_matches`' *no-wildcard* branch, which is already exactly `target == pattern || target.starts_with(&format!("{pattern}/"))` — the safe form, already in the tree. (3) Correct line 176's rationale: state the real one, which is that descendant implication is the product requirement and the boundary is enforced at the segment, not that typosquatting is impossible.

### B-4 — BLOCK — "a forged ledger's worst case is a wrong diff" is false; three concrete forgery primitives

**CONFIRMED (against the ADR's own contract shapes).**

- **ADR**: Decision 1, line 71 — *"Forgery posture: none … A forged ledger's worst case is a wrong diff … not code execution, not a bypassed consent check."* This claim is the entire justification for leaving the carrier unauthenticated. It does not hold against the ledger shape the same decision defines (line 68) and the `Plan` shape in Component Contracts (line 272).

1. **Kind confusion → attacker-chosen PATH on scope exit.** `applied` "reuses the existing `Entry {key, value, kind, separator}` serialization" — so **`kind` is read from the attacker's ledger**, not re-derived from D. Forge `PATH` as a `Constant`-kind applied entry with `priors["PATH"] = Value("/attacker/bin")` and `applied["PATH"].value` set to the shell's real current `PATH` (the forger knows it — they set the variable in that same env). The constant-exit rule is *"restore the prior only if C == L"* (line 131); the forger authored both sides of that comparison, so it holds, and leaving the project sets `PATH=/attacker/bin`. Code execution, not a wrong diff.
2. **Removal suppression → a project's PATH element becomes global and permanent.** L is the sole revert record for *arbitrary* (non-`$OCX_HOME`) elements — the ADR says so at line 127: *"arbitrary elements (project `[env]` additions) are listed in L. The prefix test doubles as the repair path when L is lost."* The prefix test only covers `$OCX_HOME`. Omit an element from L and it is never removed on leave: a project `[env]` `{type=path}` entry (`/tmp/x`, `./node_modules/.bin`) survives into every other directory for the rest of the session. **The ADR institutionalizes this without any forgery**: size-ladder rung 2 (line 69) is *"drop `applied` list-element records (lists repair via the ownership prefix test)"* — over 16 KiB, arbitrary PATH elements become structurally unremovable. A project with enough `[env]` entries reaches that rung on its own authority.
3. **`key` / `dir` as a path primitive.** The ledger carries `scopes.project = { key, dir, … }`. Nothing in the ADR says these are advisory. If any consumer builds `$OCX_HOME/state/projects/<key>/…` from the ledger's `key`, a forged `key` (`../../..`) is directory traversal into the state root, and `dir` is a lie about which project the session is in.
- **Precondition**: anyone who can set one env var in the victim's shell. Per the ADR's own line 72 that includes **a package publisher in an already-consented namespace**, because package-metadata env is ungated (see B-2) — the exact actor the consent model is bounding.
- **Fix — three normative sentences, all cheap**: (a) `kind` and `separator` are **re-derived from D**, never read from L; an L entry whose key is not in D∪(prefix-owned) is discarded. (b) `key` and `dir` are advisory only — re-derived from the CWD walk every prompt, never used to construct a path; a mismatch invalidates the whole project scope of the ledger. (c) Replace the "worst case is a wrong diff" sentence: the honest posture is *the ledger is untrusted input whose only permitted effect is to **narrow** the revert set* — never to widen it, never to choose a value, never to name a path. Rung 2 of the size ladder then needs re-stating too: dropping list-element records must mean "these elements are abandoned in place and reported", not "silently unremovable".

---

### W-1 — WARN — the config loader already has a wired project tier; `[shell.trust]`'s "structurally impossible" claim has an expiry date

**CONFIRMED.**

- **ADR**: Decision 4, line 174 — *"**Never `ocx.toml`.** This is enforced structurally, not by discipline: `ProjectConfig` and `RawProjectConfig` both carry `#[serde(deny_unknown_fields)]` … The GHSA-436v-8fw5-4mj8 class … **cannot exist** because the project file is never a candidate source for trust-control keys."*
- **Verified true today**: `crates/ocx_lib/src/project/config.rs:122` (`ProjectConfig`) and `:259` (`RawProjectConfig`); the round-trip is even asserted at `:993`.
- **But the claim is about the wrong struct.** `[shell.trust]` lands on `Config` (`crates/ocx_lib/src/config.rs`), not `ProjectConfig` — and `ConfigLoader::load_with_local_view` **already resolves a project tier into that pipeline** and deliberately parks it:
  ```rust
  // crates/ocx_lib/src/config/loader.rs:145
  let _project_path = Self::project_path(inputs.cwd, inputs.explicit_project_path).await?;
  ```
  with the comment *"Phase 1 only wires error propagation — the returned path itself is consumed in later phases once the project-config schema lands."* `project_path` resolves `ocx.toml` (`loader.rs:518-585`, `walk_for_project_file` at `:634`). So the untrusted tier is pre-wired into the trust-bearing struct's loader, one commit away from being read, and `Config` has **no `deny_unknown_fields` anywhere in its tree** (the ADR itself requires this at line 274 for fleet forward-compat) — meaning an `ocx.toml`-sourced `[shell]` block would parse silently rather than erroring.
- **Impact**: this is precisely the mise GHSA shape, pre-staged. Not exploitable today; exploitable the day the parked line is consumed, with nothing in the tree to stop it.
- **Fix**: convert the prose claim into a guard that can go red. When `[shell]` lands on `Config`, add an explicit strip of `shell` from any project-tier contribution, in the same place and idiom as `guard_managed_sigstore_trust` (`config/loader.rs:431`), plus a test that a project-tier `Config` fold cannot contribute `[shell.trust]`. Also state the `deny_unknown_fields` guarantee as an invariant with a test, not as an observation about two line numbers.

### W-2 — WARN — the digest-pin gate on `[shell.trust]` is a single load-bearing guard, and its WARN is emitted where nothing can see it

**CONFIRMED.**

- **ADR**: Decision 7 (lines 254-256) — `[shell.trust]` merges *"only when the `[managed] source` is digest-pinned. Otherwise it is stripped with a WARN naming the reason,"* reusing `guard_managed_sigstore_trust`'s home and idiom.
- **The precedent holds mechanically**: `guard_managed_sigstore_trust` (`crates/ocx_lib/src/config/loader.rs:431-457`) is called from `fold_managed_tier` at `:400` immediately before `accumulator.merge(parsed)`, and `:3098` in tests. Extending it is the right call.
- **Two gaps.**
  1. **The warning has no reader on this path.** Decision 5 puts the config read inside `ocx self activate`, and the ADR itself establishes (line 229) that *"the shims discard the binary's stderr (`2>/dev/null`, `shims.rs:63,103,105,146,239`)"* — verified: `shims.rs:63`, `:78`, `:103`, `:105`, `:239`. `log::warn!` goes to stderr. So an operator who ships `[shell.trust]` from an unpinned tag gets **silence** on the exact path where the strip matters, and the ADR's own rule about routing user-visible hook output through `printf … >&2` inside the eval'd script is not carried over to this warning.
  2. **One guard, where the sibling surface has two.** The ADR's own Decision 4 line 177 says *"Precedence: union, never override … There is no untrusted tier in the union."* The codebase disagrees with that sentence: `crates/ocx_lib/src/trust.rs:36-38` describes *"the untrusted managed-config payload"* in as many words, and Decision 7 gating that tier concedes the point. Because the union is unordered by construction, the digest-pin gate is the **only** thing between an unpinned managed payload and a PATH-front trust grant — whereas `[[trust.policy]]`, the surface being cited as precedent, additionally has `system_locked` admission authority and operator-over-project tiering (`trust.rs:20-40`, `resolve` at `:747`).
- **Fix**: (a) route the strip's reason through the same `printf … >&2`-inside-the-script channel the rest of the hook uses, or record it where `ocx about` can surface it; (b) delete or qualify the "no untrusted tier in the union" sentence — it contradicts Decision 7 four sections later; (c) demand a red+green test for the gate specifically (unpinned source ⇒ `[shell.trust]` absent from the merged config), per the ADR's own "every check demonstrated red and green" rule.

### W-3 — WARN — OD-3's named mitigation is opt-in, off by default, and (absent an operator policy) authored by the attacker

**CONFIRMED.**

- **ADR**: Decision 8, line 266 — *"`[[trust.policy]]` plus `ocx package verify` is the publisher-compromise control, and it is signature-based, which is the correct instrument."* Non-Functional Requirements (line 287) repeats it as *"Residual: Decision 8, documented, mitigated by signature verification."*
- **What the code actually gives you** (`crates/ocx_lib/src/package_manager/tasks/auto_verify.rs:22-36`, gate rules verbatim):
  1. *"No `AutoVerify` configured (no trust policies) → **no-op**."* — the default state of every user who has not written a policy.
  3. *"No matching policy → INFO log, **install proceeds** (opt-in trust model)."*
  Resolution is `trust::resolve_tiered(operator_policies, project_policies, target)` (`auto_verify.rs:170`). Per `trust.rs:20-23`, when **no operator policy matches**, the **project `ocx.toml` tier applies** and *"may add trust for scopes the operator has not governed."*
- **Consequences for the victim this ADR is protecting** — someone who cloned a repo and has no operator trust config:
  - The hook never verifies anything: it is compose-only (ADR line 154), so the control is not on the per-prompt path at all. It sits on the later `ocx pull`/`install` that brings the new digest into the store.
  - With no operator policy, the control is a no-op. With no operator policy but a repo-supplied `[[trust.policy]]`, verification runs and attests **the repo author's own identity** — a pass that proves nothing to the victim.
  - Operator-authored policies *are* correctly authoritative (`resolve_tiered`), so the "project weakens an operator pin" bypass does **not** exist. That half is sound.
- **Also**: the ADR's phrasing *"whoever can publish gets PATH-front code at the victim's next prompt with no signal"* (line 262) overstates the residual. `ocx.lock` pins the per-platform leaf digest (`crates/ocx_lib/src/project/lock.rs:163-176`, `LockedTool { repository, platforms: BTreeMap<String, Digest> }`), so a swap reaches PATH only after a lock-changing event (`git pull`, `ocx update`) **and** a subsequent install — which is exactly the seam auto-verify sits on. Say that; it makes the residual smaller and the mitigation's placement honest.
- **Fix**: rewrite Decision 8's last paragraph to state that the mitigation is **opt-in and default-off**, name the enabling step (an operator-tier `[[trust.policy]]` in `config.toml`, not `ocx.toml`), and state that a project-tier policy is not a control for this threat. As written it reads as an active control and it is a deferral.

### W-4 — WARN — the source-set predicate's identity is underspecified, and the new sweep drops the guards Option C was chosen to preserve

**CONFIRMED.**

1. **`sources` granularity is never pinned.** Decision 4 (line 156) defines `sources: BTreeSet<String>` as *"the registries+namespaces the lock resolves against"* and gives one worked example (`ghcr.io/acme → ghcr.io/evil` triggers). Nothing states the normalization. At registry granularity (`ghcr.io`) the predicate is nearly vacuous — consenting to one GHCR project consents to all of GHCR. At full-repository granularity it re-prompts on every ordinary tool addition, which is the ceremony fatigue Decision 4 explicitly rejects. This single choice sets the strength of the only default-on control in the design (see W-3). It must be normative in the ADR, with the derivation named: `LockedTool.repository: Identifier` (`project/lock.rs:170`) truncated to `<registry>/<first-segment>`, and — stated explicitly — the **physical** address the lock records, not the logical identifier the user typed, so an index repointing (`adr_index_indirection.md`) is visible to the predicate rather than invisible to it.
2. **The state sweep re-introduces what Option C avoided.** Decision 2 rejects Option B partly because it *"turns the prune from `symlink::remove` into `remove_dir_all` inside the module carrying the SEC-1 and TOCTOU guards"* (line 92) — then Decision 2's Contract (line 113) specifies exactly a `remove_dir_all` sweep, in `clean.rs`, over `state/projects/<key>/`, with **none** of those guards restated: no three-state probe, no pre-removal re-probe (the CODEX-BLOCK-1 pattern at `project/registry.rs:105-133`), no `symlink_metadata` check that `<key>` is a real directory rather than a symlink, no `.tmp-*` staging-name skip. `clean.rs:494` already uses `tokio::fs::remove_dir_all`. State the guards, or state why a state sweep is entitled to fewer than the ledger prune it is modelled on.
3. **Two directories named `projects`.** `$OCX_HOME/projects/` (ledger, symlinks, GC truth) and `$OCX_HOME/state/projects/` (consent stamps) share a name *and* a key space while having opposite lifetimes and opposite failure modes. Worth one sentence in the ADR and one in `subsystem-file-structure.md`.

### S-1 — SUGGEST — say which side of the `paths` comparison is canonicalized

Decision 4 line 175 says `paths` is *"exact canonical directory, matched on the canonicalized path"* — ambiguous about the **whitelist entry**. Canonicalizing entries at read time makes the grant follow a symlink an attacker may control on the parent (`/workspaces/repo → /tmp/evil`, needing only write on `/workspaces`); comparing them literally never matches a symlinked checkout and is silently inert. Both are defensible; only one can be implemented, and the failure modes are opposite. Pick one and say so. (The project side is already correct: `dunce::canonicalize` at `project/registry.rs:311`, and `walk_for_project_file` rejects a symlinked `ocx.toml` at `config/loader.rs:652-657`.)

### S-2 — SUGGEST — the 64-bit key is the only project identity unless `evaluate` checks `project_dir`

`ReferenceManager::name_for_path` is SHA-256 **truncated to 8 bytes** (`crates/ocx_lib/src/reference_manager.rs:59-63`) — a 16-hex, 64-bit name. Second preimage is out of reach, so this is not a live attack; but `ConsentStamp` already carries `project_dir: PathBuf` (ADR line 277) and the ADR never says `evaluate` compares it. One line — *"`evaluate` rejects a stamp whose `project_dir` is not the canonical directory under evaluation"* — makes the key a lookup index rather than the identity, and costs nothing.

### S-3 — SUGGEST — make "consent before parse" normative, not just "consent before apply"

Decision 4's event list has the right order (*"enter (stamp check → compose → apply)"*, line 141), but Decision 4's headline (*"Otherwise: **zero env change**"*, line 152) is satisfiable by compose-then-discard, which would have deserialized the untrusted `ocx.toml` first. The residual risk is small — project `[env]` is literal-only with no interpolation (`crates/ocx_lib/src/project/env.rs:27`) — but the mise lesson is about ordering, and the cheap fix is to say it once: **the only project-supplied bytes read before consent is established are the CWD walk's stat calls and the `ocx.lock` parse the source-set predicate requires; `ProjectConfig` deserialization happens after.** That also makes the lock-parse carve-out (line 160) an explicit, bounded exception rather than an aside.

---

## Surfaces probed and found sound

Each was attacked, not skimmed; each is listed with the evidence that closed it.

| # | Surface | Why it holds |
|---|---|---|
| 1 | `__OCX_ENV_STATE` needs no new reservation | `is_reserved_ocx_key` (`crates/ocx_lib/src/env.rs:1143-1146`) uppercases then matches `OCX_`/`__OCX_`, so it is case-insensitive by construction — the Windows case-folding argument in its doc comment is real. Tests at `env.rs:1587-1604` cover `__OCX_TESTING_INSTALL_BINARY` and lowercase `__ocx_testing_x`. The three enforced channels (`options/env_override.rs:170`, `env.rs:1406`, `project/env.rs:158`) do gate it. Correct for those three; the fourth is B-2. |
| 2 | base64url as the carrier encoding | Not decoration, as the ADR says. `escape_value`'s `Batch` arm (`shell.rs:520-526`) does not escape `"`, and elvish rejects `\$`/`` \` `` in double-quoted strings as a **parse error** (`shell.rs:495-497`) — a raw JSON payload would hit both. An opaque alphabet removes the whole class in one move. Rejecting compression (Choose Boring Technology) is right for a payload measured in low KB. |
| 3 | Withdrawing the `OCX_ENV → __OCX_ENV` rename | Verified against the live contract: `apply_ocx_config` does an **unconditional** `self.remove(keys::OCX_ENV)` (`env.rs:501`) with a doc comment stating exactly why (a stale inherited `OCX_ENV` must never reach a child), and the payload is written after, in `apply_child_env`. The ADR's read is accurate; the rename would have broken a live seam for nothing. |
| 4 | No `OCX_ACTIVATED`-style session guard reintroduced | The design leans on idempotent emission instead, which is the constraint the removal of that guard imposed. Nothing in the ADR takes it back. |
| 5 | The `export_path` template `remove_path_element` copies | Already hardened across all 10 arms and tested: pattern-quoting inside `${//}` to force literal match (`shell.rs:174-176`), `ENVIRON` rather than `awk -v` to avoid backslash decoding (`:186-190`), fish exact `test` compare, PowerShell ordinal split, elvish raw single-quoted, nushell `$`/`(`/`)` neutralization. Injection tests at `shell.rs:793-972` cover command substitution, backtick, backslash, history expansion and `$` per shell. Both hazards the ADR names (zsh glob over-match, bash `${//}` pattern escaping) are **already solved** here. Only the escaper the ADR *names* is wrong (B-1). |
| 6 | `Shell::Batch` ⇒ `None` for element removal | Carried correctly from `export_list`. The underlying reason is real: `cmd.exe`'s only substring-replace primitive is case-insensitive with no case-sensitive form, so it cannot express a case-sensitive element delete. Emitting nothing beats emitting a wrong statement. |
| 7 | Placement of the managed-tier gate | `guard_managed_sigstore_trust` is invoked inside `fold_managed_tier` at `config/loader.rs:400`, immediately before `accumulator.merge(parsed)` — the strip cannot be sidestepped by merge ordering. Reusing its home is the right call; the gaps are W-2's (visibility, single-guard), not placement. |
| 8 | Discovered `config.toml` tiers cannot be redirected by symlink | `discover_paths` probes with `symlink_metadata` and drops any symlinked candidate with a warning (`config/loader.rs:496-515`). An attacker who can create `/etc/ocx/config.toml` or `$OCX_HOME/config.toml` as a link to their own file gets nothing. `[shell.trust]` inherits this for free. |
| 9 | Consent-stamp write mechanics | `write_bytes_atomic` (`crates/ocx_lib/src/utility/fs.rs:312-331`) stages a tempfile **in the target directory** with mode `0600` on Unix and persists by rename. Replace-never-edit is what the ADR specifies (line 115), and it is what the helper does. The `lock_scoped`-not-sidecar note is consistent with the Locking Policy. |
| 10 | GC carry-over from the projects ledger | `ProbeResult::{Live,Dead,Unknown}` with `Unknown` never treated as dead (`project/registry.rs:92-133`), plus the pre-removal re-probe. The ADR's "an `Unknown` project is live for that run, so its state is retained" is an accurate read. The `$OCX_HOME` sweep carve-out is *required*, not optional: `register` no-ops on `$OCX_HOME` via device+inode identity (`registry.rs:319-324`, ARCH-1b), so that key genuinely never appears in `live_projects()`. |
| 11 | `OCX_TRUST_*` from a hostile **parent process** declared out of scope | Correct, and stronger than the ADR argues: a parent that can set those can also set `OCX_CONFIG` to an arbitrary file (loader tier 4, `config/loader.rs:106-110`) or simply set `PATH`. There is no privilege to escalate. Matches every tool in the research. The *package-metadata* channel is a different actor and is B-2. |
| 12 | Union-not-override precedence for the whitelist | Following git is right, and the reason mise's most-specific-wins became GHSA-436v-8fw5-4mj8 is correctly identified. (The accompanying sentence "there is no untrusted tier in the union" is the part that needs correcting — W-2.) |
| 13 | Comma separator for `OCX_TRUST_NAMESPACES` | A real constraint, not fussiness: an OCI registry may carry a port (`localhost:5000/acme/*`), so the OS PATH separator is unusable on Unix for namespace strings. Splitting the two env vars on different separators is the honest answer. |
| 14 | Every default fails toward "do nothing" | Absent state dir ⇒ no consent (line 94); unreadable/unparseable lock ⇒ no activation (line 160); malformed ledger ⇒ rebuild from truth, never guess-unset a constant (line 70); an emitter that cannot express an operation returns `None` rather than a broken line. For a feature whose failure mode is "wrong thing on PATH", every one of these points the right way. |
| 15 | `[shell]` in `ocx.toml` is a hard parse error **today** | `#[serde(deny_unknown_fields)]` on `ProjectConfig` (`project/config.rs:122`) and `RawProjectConfig` (`:259`), with a nested-table round-trip test at `:968-999`. The observation is true. W-1 is about its durability, not its truth. |
| 16 | "No emitted snippet may ever call bare `ocx`" | Correct and non-obvious — `command -v ocx` finds shell functions, so a bare call inside the eval'd stream would run the wrapper inside a command substitution and capture its output into the env stream. The `$_ocx_bin` pattern the rule mandates is already what every shim does (`setup/shims.rs:63,103,105,239`). The regression test it asks for is the right shape. |
| 17 | Project-tier trust policy cannot weaken an operator pin | `trust::resolve_tiered` is operator-authoritative (`crates/ocx_lib/src/trust.rs:20-23`, `:786-800`), and a system-scope policy is admission-authoritative via `system_locked` (`resolve`, `trust.rs:747-785`) — a lower tier can neither outbid it nor join its ANY-of set. The "equal-scope array-append is a signer-enrollment channel" reasoning is already in the code. This half of Decision 8 is sound; W-3 is about the *default-off* half. |
| 18 | `ocx.lock` pins per-platform leaf digests | `LockedTool { repository: Identifier, platforms: BTreeMap<String, Digest> }` (`project/lock.rs:163-176`) with `deny_unknown_fields` across the lock tree (`:107,138,162`). A publisher cannot reach a locked project's PATH without a lock-changing event first — which makes the OD-3 residual materially smaller than Decision 8 states (see W-3). |
| 19 | The `Unknown`-probe fail-closed contract for consent state | Retaining state for an indeterminate project is the correct direction here too: the failure mode of over-retention is a stale consent stamp for a project that still exists, versus deleting a live project's stamp (which is merely inert). Both directions are safe; the ADR picked the one that matches the ledger. |
