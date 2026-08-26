# Adversarial Quality Review — `adr_shell_env_overhaul.md`

Reviewer: quality / trade-off honesty, adversarial. Findings ranked most-severe first.
Status: COMPLETE — 12 findings + survivor list.

## Q-1 — `[shell.trust]` reinvents `ScopeSpec`, which already ships in this repo — and reintroduces the one forward-compat hazard `trust.rs` documents as forbidden — CONFIRMED

**Claim attacked.** Decision 4's whitelist grammar ("`namespaces` is a namespace prefix with a single trailing `/*`"), the Component Contract `ShellTrust { paths: Vec<PathBuf>, namespaces: Vec<String> }` with "**No `deny_unknown_fields` anywhere in the tree** (fleet forward-compat)", and "Precedence: union, never override … no precedence logic is needed and none is added". Underwritten by "Key insight: every primitive here is precedented."

**Why it fails.** The primitive is precedented *inside this repository*, and neither the ADR nor `research_trust_whitelist_grammar.md` mentions it. `crates/ocx_lib/src/trust.rs:247` ships `ScopeSpec`, matched against canonical `registry/repository` targets, with exactly the proposed grammar (`scope = "ghcr.io/acme/*"`, `config/loader.rs:1523`), plus two things the flat `Vec<String>` cannot express and the ADR silently forgoes:

- `ScopeSpec::Set { include, exclude }` — carve-outs. A fleet that pre-trusts `ocx.sh/acme/*` but must exclude one compromised namespace has no spelling under Decision 4.
- `ScopeSpec::specificity_for` — per-target rank resolution. The ADR replaces it with union-only and asserts "no precedence logic is needed", against a shipped counter-example in the same config file.

Worse, `trust.rs:252-257` documents *why* trust-bearing tables are the exception to this repo's forward-compat tolerance, verbatim: "A table carrying only keys a newer ocx understands is therefore refused, not read as an accidental catch-all — **the one place the fleet forward-compat tolerance stops**, because here dropping the unknown key would *widen* trust rather than narrow it." Decision 4 puts a trust-granting table under a blanket "no `deny_unknown_fields` anywhere in the tree" and reopens exactly that hole.

This also contradicts Decision 8's own doctrine — "Conflating them would produce a second, weaker trust system" — which is precisely what a second, weaker prefix grammar for "whose binaries may reach my PATH", in the same file, is.

**Scenario.** Operator publishes managed `[shell.trust] namespaces = ["ocx.sh/acme/*"]` plus a future narrowing key (`exclude`, `require_signed`) that a fleet host's older ocx does not know. The older ocx drops the unknown key silently and activates on the full namespace. Under `[[trust.policy]]`'s shipped rule that same payload is refused.

**Fix.** Reuse `ScopeSpec` for `namespaces`; inherit its deserialize discipline for the trust-bearing table; drop the parallel union-only precedence claim or justify it against `specificity_for`.

## Q-2 — The per-prompt no-op budget is ~77% consumed by process spawn before any ocx work; the number is borrowed from mise, not measured on ocx — CONFIRMED (measured)

**Claim attacked.** D4 ("The no-op path is stat-only; the number is asserted in CI") and NFR Latency ("no-op path ≤ **5 ms** (mise ships ~4 ms on the same mtime shape), asserted by a CI benchmark with a hard fail").

**Why it fails.** "Stat-only" describes the work *inside* the process and omits the process. Decision 5 layer 2 re-invokes the binary every prompt (`self activate --reconcile`), and `crates/ocx_cli/src/main.rs:18` is `#[tokio::main]` — every invocation execs a 54 MB binary and builds a multi-thread Tokio runtime (worker threads = core count) before dispatch reaches any stat.

Measured on this machine (release build, warm cache, 32 cores, WSL2), 20 iterations each:

| Invocation | Mean |
|---|---|
| `ocx --version` (does nothing) | **3.85 ms** |
| `ocx self activate --shell bash --no-completion` | **3.80 ms** |

That is the floor, on fast hardware, before the CWD upward walk, the watch-set stats, ledger decode and script emission the ADR adds on top. ~1.2 ms of a 5 ms budget remains. On macOS (slower dyld), on a 2-core CI runner (Tokio still spawns, page cache colder), and above all on Windows — where process creation is an order of magnitude more expensive and where the ADR calls PowerShell "first-class" — the exec alone exceeds the budget.

The 4 ms is mise's measured number for mise's binary; importing it as ocx's budget is the "verified without citing evidence" pattern `quality-core.md` classifies Block-tier. Compounding it: the Validation section demands "every check demonstrated red and green", and a wall-clock budget assert on shared CI runners is the canonical flaky gate — the ADR does not say how red is produced.

**Scenario.** Windows/pwsh user, default-on hook, every prompt pays a `CreateProcess` of a 54 MB binary to learn that nothing changed.

**Fix.** The exec is avoidable on the no-op path. bash/zsh/ksh `[[ file -nt stamp ]]` is a builtin mtime comparison with zero exec; the emitted hook can carry the watch-set paths and short-circuit shell-side, execing ocx only when something is newer. That path is never considered in the ADR. Failing that, measure ocx's own floor per platform and state the budget as floor + work.

## Q-3 — NAMED QUESTION (Decision 3): the apply rule is deliberate; its interaction with the exit guard is accidental, and it converts a user's `export` into an ocx-owned value that ocx then *unsets* — CONFIRMED

**Claim attacked.** "Apply/update: set to D unconditionally (project wins while inside the project)" versus "Exit: restore the prior **only if C == L**; otherwise leave C. Never clobber a user's mid-session override on the way out — the single most-disliked direnv behavior." And D1: "never clobber a foreign write … must be structurally impossible, not merely avoided."

**Why it fails.** The unconditional apply is stated with a rationale, so *as a rule* it is deliberate. What is accidental is the state machine it produces with the exit guard, because priors are captured on *scope entry* only:

1. Enter project. `L = {applied: D1, prior: Unset}`, `C = D1`.
2. User runs `export JAVA_HOME=/my/jdk`. `C = /my/jdk`, `C ≠ L`. Under the exit rule this value is sacred.
3. Anything recomposes — `git pull` touches `ocx.lock`, `ocx update` changes the binary version (both are in the fingerprint watch set), or a project switch A→B→A. Apply sets `JAVA_HOME = D` unconditionally. The override is gone, and `L.applied` is now `D` while `L.prior` is still `Unset` from step 1.
4. `C == L` again. On leave, ocx **unsets** `JAVA_HOME`.

So the override is not merely clobbered mid-session — one recompose launders it into an ocx-owned value, and the exit guard, whose entire purpose is to protect it, now provably fires against it. The end state is strictly worse than direnv's: direnv restores a stale value, ocx removes a variable the user set by hand. The ADR never notices the transition, which is why I read it as accidental rather than accepted.

This also falsifies D1 as written. Foreign writes are structurally invisible only for keys outside D ∪ L; for a constant *inside* D, clobbering a foreign write is the specified behavior on every recompose. D1 is true of list-kind vars and of the exit path, and is asserted absolutely.

**Precedent.** mise shipped this exact apply rule in 2026.8.0 and reverted it in 2026.8.9 ([jdx/mise#12094](https://github.com/jdx/mise/issues/12094)) because runtime overrides were reverted every prompt. The ADR cites mise as validation for the typed diff and does not carry this correction across. The fingerprint fast path narrows the blast radius from "every prompt" to "every recompose" — it does not change the outcome, and it makes the failure *intermittent*, which is worse to diagnose.

**Minimum fix.** Re-capture the prior when apply overwrites a value ocx did not write (`C ≠ L.applied` ⇒ `L.prior := C`). Two lines in the plan step, and it makes the exit guard's promise true. Preferably also: adopt mise's post-revert rule and do not overwrite a key whose current value diverges from what ocx last wrote.

**Secondary, PLAUSIBLE.** "Coincidence (`C ≠ L` but `C == D`): claim silently, prior = C" restores `D` on exit, leaving the project's value set in the parent scope after leaving the project. Defensible (the user typed it) but unstated as a leak.

## Q-4 — Decision 2 relocates the hazard rather than removing it: the sweep collects consent for every project the ledger cannot see — CONFIRMED

**Claim attacked.** Option C's trade-off row "Risk to the hardened liveness path (×3) | **none** — ledger untouched", and "GC: `ocx clean` … additionally removes any `state/projects/<key>/` whose `<key>` is not in `{ name_for_path(dir) | dir ∈ live_projects() }`."

**Why it fails.** The row is literally true and answers a narrower question than its header. Option B's hazard was *inside* `probe_live_target`; Option C moves it one layer out, into a new consumer that is coupled to the ledger's population rule — and that rule is narrower than the ADR's consent-writer list.

Ledger registration has exactly two call sites (`project/lock.rs:448`, `project/mutation.rs:373`, both via `register_project_dir_best_effort`): a lock **save** or a mutation **commit**. `live_projects()` further requires `<target>/ocx.lock` to exist (`registry.rs:127-131`). Decision 4 writes a consent stamp on `add, remove, lock, update, pull, run`. Those sets do not coincide:

- A project with `ocx.toml` `[env]` and **no lock**. Decision 3 makes this a first-class case ("`[env]` applies on its own authority independently of the lock"). It can never be in the ledger. Its consent stamp is deleted by the next `ocx clean`, always.
- `ocx run` / `ocx pull` against an already-current lock — stamp written, no lock save, no ledger entry.
- `--frozen` forbids lock writes by contract; the stamp it earns is unbacked.
- A project whose lock is transiently absent (mid-`git checkout`, mid-rebase) probes `Dead` → ledger link pruned → consent stamp swept on that same run.

**Scenario.** Developer works in an `[env]`-only project daily. Every `ocx clean` silently revokes consent; the next `cd` in produces the "inert, one hint line" path with no explanation, and the tooling they had is gone from PATH until they run an explicit ocx command. Repeat forever. This is the ceremony D5 exists to abolish, reintroduced on a GC schedule.

The failure direction is fail-safe (inert, not activated), so this is a usability and durability defect, not a security one. But the ADR sells "GC (×3)" as Option C's decisive merit, and the GC it buys is wrong for the data it stores.

**Also.** The sweep re-derives `name_for_path(dir)` from the returned path when the ledger already stores that key *as the entry filename*. Two derivations of one value, and they can diverge: the entry name is hashed at register time; `live_projects()` re-canonicalizes at read time, so a changed parent symlink or a case-normalizing filesystem yields a different hash for the same live project — and its state directory is swept while its ledger link survives. Expose `live_project_keys()` instead of re-hashing.

## Q-5 — The Decision 2 option table is scored so the chosen option's implementation work counts as merit and the rejected one is scored unimplemented — CONFIRMED

**Claim attacked.** The Decision 2 criteria table, specifically the two ×3 rows that carry the verdict: "Collapses the key schemes" (A: "no — a third scheme") and "GC" (A: "**none** — `state/` is not GC-walked, confirmed by grep"; C: "sweep against `live_projects()`, one call site in `clean.rs`").

**Why it fails.** Both rows dissolve once the ADR's own decisions are applied uniformly.

- *Key schemes.* Decision 2's first line fixes one key derivation — `name_for_path(canonicalized_project_dir)` — before the table is read. Under that rule, `state/activation-consent/<key>` and `state/projects/<key>/consent.json` use the **same key**. The remaining difference is subsystem-first versus project-first directory nesting, i.e. layout taste. Scoring A as "a third scheme" imports the brief's pre-decision framing into a table that the decision has already settled.
- *GC.* `state/` is not GC-walked for **either** option — that is a property of `state/`, not of the layout (`subsystem-file-structure.md:236`: "**GC:** not walked by `ocx clean`"). C gets GC because the ADR writes a sweep for it. The identical sweep over `state/activation-consent/` is the same call site and the same `live_projects()` call. A is scored as-if-unimplemented; C as-if-implemented.

Six of the table's ~13 weight points therefore do not discriminate. What is left that genuinely does: B's dual-shape read path (real, decisive), D's project-writable stamp (real, fatal). The A-versus-C comparison — the only one the chosen option actually wins — is decided on rows that do not separate them.

**Separately, the `state/` contract is cited selectively.** The ADR quotes "safe to delete at any time without integrity loss" as evidence that `state/` "fits a consent stamp exactly", from a definitional contract whose adjacent bullets read "ephemeral", "TTL-bound per subsystem", and "**not walked** by `ocx clean`". Decision 2 amends that last bullet and the ADR does not say so; `subsystem-file-structure.md` is listed in the doc surfaces only as a "state layout row".

## Q-6 — The mise-CVE closure rests on a `deny_unknown_fields` whose documented purpose is catching typos, and which the same ADR argues against one section later — CONFIRMED

**Claim attacked.** "**Never `ocx.toml`.** This is enforced structurally, not by discipline: `ProjectConfig` and `RawProjectConfig` both carry `#[serde(deny_unknown_fields)]` … so a `[shell]` block in `ocx.toml` is a hard parse error … The GHSA-436v-8fw5-4mj8 class … **cannot exist**."

**Why it fails.** The attribute exists, at `project/config.rs:122`, and its docstring states its purpose (`:113-115`): "enforced at the struct level so **schema drift in consumer `ocx.toml` files surfaces as a parse error** rather than silent ignore." It is a typo detector. Its removal would be an ergonomics regression to everyone who currently reasons about it — and a silent, total security regression to Decision 4, in a different file, with nothing in either file recording the coupling.

The pressure to remove it is not hypothetical: it is the argument this very ADR makes. `config/managed.rs:23`, `config/patch.rs:21` and `config/registry.rs:19` each omit `deny_unknown_fields` and each cite fleet forward-compat; the Component Contract mandates "**No `deny_unknown_fields` anywhere in the tree**" for `Config`. `ProjectConfig` is the outlier, and the first person who applies the ADR's own forward-compat doctrine to `ocx.toml` reopens GHSA-436v-8fw5-4mj8 without ever opening the ADR.

"Structurally, not by discipline" is the inverted claim: a guard whose premise lives in another file, is undocumented as load-bearing, and has no test, *is* discipline.

**Scenario.** A later ADR relaxes `RawProjectConfig` to tolerate unknown keys for forward-compat (the exact rationale three sibling config structs already use). `[shell.trust]` in a hostile `ocx.toml` now parses as an ignored key today — and the moment any code path merges project config into `Config`, it grants trust.

**Gap.** The Validation section tests "whitelist grammar (exact-path non-match on a sibling directory; namespace `/*` boundary)" and does **not** test that `[shell]` in `ocx.toml` is refused. The one assertion that pins the security claim is absent.

## Q-7 — The 16 KiB cap: the justifying arithmetic is wrong by 5×, the cap silently doubled, and the degradation ladder was silently reversed to shed the unrepairable rung first — CONFIRMED

**Claim attacked.** "16 KiB clears every platform floor **by an order of magnitude** (Windows XP/2003 caps the whole block at 32767 chars; Linux `MAX_ARG_STRLEN` is 128 KiB per string)" and the size ladder "drop `priors` → drop `applied` list-element records → omit the variable entirely".

**Why it fails, three ways.**

1. **The arithmetic.** 16 KiB = 16384 against the cited 32767-char *whole block* is a factor of **2.0**, not an order of magnitude — and it is a factor of 2 against a limit shared with every other variable in the block. A carrier permitted to consume half the entire environment block is not "clearing the floor"; it is the floor. The only cited limit the claim holds for is the Linux one, which is not the binding constraint.
2. **The cap doubled with no reason given.** The superseded ADR set 8 KiB on the same Windows reasoning; this ADR sets 16 KiB. Nothing in Decision 1 acknowledges the change, and Decision 1's own payload estimate ("hundreds of bytes to low KB") argues for the smaller number, not the larger.
3. **The ladder is inverted relative to the predecessor's, and the ADR supplies the argument against its own ordering.** The superseded ADR shed list-element records first, priors second. This one sheds **priors first**. Decision 3 states that lists are repairable without the ledger ("Ownership: elements under `$OCX_HOME` are self-identifying by prefix … The prefix test doubles as the repair path when L is lost") while priors are the one thing that cannot be reconstructed from any other source. The new ordering discards the unrecoverable datum first and keeps the recoverable one longer. That reversal of an adversarially-reviewed decision is unmarked in the Changelog.

**Scenario.** A large monorepo pushes the carrier over cap. Rung 1 fires, priors are dropped, and on scope exit ocx can no longer restore anything — every constant it set stays set in the parent shell, permanently, with no signal. Under the predecessor's ordering the same overflow would have dropped list records and repaired them by prefix.

**Fix.** Restore the predecessor's ladder order, or state why the reversal is right. Correct the "order of magnitude" sentence, and re-justify 16 KiB against a 32767-char block rather than against `MAX_ARG_STRLEN`.

## Q-8 — "How Would We Reverse This?" contradicts Decision 5, and the hidden `--reconcile` flag is an unversioned cross-version contract — CONFIRMED (contradiction) / PLAUSIBLE (consequence)

**Claim attacked.** Reversal level 1: "`[shell] hook = false`, or `OCX_NO_HOOK=1`, or `ocx self setup --no-hook`. Takes effect at the next shell start; **the reconciler reverts what it applied on the way out**." Level 4: "`__OCX_ENV_STATE` evaporates with the sessions that hold it: no persisted format, no migration."

**Why it fails.**

- **Level 1 is self-contradictory across two shells.** Decision 5 is explicit: "hook presence in a session is decided **once, at startup**." So in the *running* shell the disable is never observed and nothing is reverted; in the *next* shell there is nothing to revert, because no state was applied. The clause "the reconciler reverts what it applied on the way out" describes a third shell that does not exist. The honest statement is: disabling the hook strands the applied environment in every already-running shell until it exits. That is fine — it just is not "cheap, at four levels" without saying so.
- **The reversal section reasons about code and never about deployed session state.** The question the team asked — does reversal survive state already written into users' shells and `$OCX_HOME` — is answered for `__OCX_ENV_STATE` (correctly: it evaporates) and skipped for everything else: `[shell]` keys written into user `config.toml` by `self setup --hook`; `[shell] hook = false` published into a *fleet's* managed payload at level 2, which the operator must then un-publish; and `state/projects/<key>/consent.json` files that become orphans in a tree the ADR itself says is no longer swept once the sweep code is removed.
- **`--reconcile` is a version contract dressed as plumbing.** A running shell's hook body was emitted by binary X and invokes whatever `$OCX_HOME/.../current` points at now — which `self update` swaps underneath it, and which a downgrade or a `OCX_BINARY_PIN` can make *older*. After removal (level 4) or a rollback, every prompt in every running shell execs a binary that rejects `--reconcile`. The ADR devotes a full paragraph to exactly this mixed-version reasoning for `OCX_ENV` and does not apply it here. Whether the prompt then prints a clap usage error at every prompt depends on stderr handling in the emitted hook body, which the ADR does not specify — hence PLAUSIBLE on consequence, CONFIRMED on the unaddressed surface.

**Scenario.** `ocx self update` rolls a release back to a pre-hook binary. Every open terminal keeps calling `self activate --reconcile` once per prompt against a binary that has never heard of it.

## Q-9 — Complexity budget: five items are not load-bearing for "replace direnv" — CONFIRMED

The reconciler, the `remove_path_element` 10-shell matrix, the ledger, the consent stamp, the enablement ladder (owner-specified in the brief) and the shim invariant all earn their place. These do not:

| Cut / defer | Why it is not load-bearing | Cost of cutting |
|---|---|---|
| `[shell.trust] paths` + `OCX_TRUST_PATHS` | The ADR itself supplies the fallback in NEEDS CLARIFICATION 1: "If only one may ship, ship `namespaces` and serve devcontainers by having the image run one explicit `ocx lock`/`ocx pull` to earn a stamp." One image build-step replaces a second grammar, a second env var, a second precedence story and the sibling-typosquat argument that the ADR spends a paragraph defending against | one line in devcontainer docs |
| `[shell.trust]` in the managed tier + the digest-pin gate (Decision 7's second half) | Fleet-only, zero users at v1, and it is the half that requires a new security gate. `[shell] hook` merging unconditionally is the part with a real operator use case ("hooks off on build agents") | fleet pre-trust waits one release |
| The 3-rung size-degradation ladder | Decision 1 measures the payload at "hundreds of bytes to low KB" against a 16 KiB cap, then uses that same measurement to reject compression as "unjustified complexity (Choose Boring Technology) … revisit only if measurement shows payloads approaching the cap". The identical argument defeats the ladder, and the ADR applies it to one and not the other | cap + "treat as absent" is the whole mechanism; add the ladder if the cap is ever hit |
| The `state/projects/` sweep + `project_state_root()` + the `$OCX_HOME` carve-out | Buys GC for O(projects) files of a few hundred bytes each, and buys it wrongly (Q-4). `state/` has never been GC-walked and the tree is documented as "safe to delete" | consent stamps accumulate; `ocx clean --state` is already named as the v2 answer in `subsystem-file-structure.md` |
| Verbosity `silent \| summary \| full` | Appears exactly once, in an NFR bullet, with no flag, no config key, no env var and no default named. It is a knob nobody can turn | ship the summary line |

Net: ~40% of the surface area (a second trust grammar, a managed-tier gate, a GC sweep, a degradation ladder, a verbosity tri-state) can be deferred without weakening "replace direnv on 10 shells with a reconciler that never clobbers a foreign write".

## Q-10 — Superseding census: six mitigations carried, two dropped silently, one weakened — CONFIRMED

The superseded ADR's Changelog row (2026-08-02, "Adversarial review round 1") lists what a prior round bought. Verified line by line against the new ADR:

| Mitigation | Status |
|---|---|
| Ledger spoof channel + reserved-key gate | **Carried**, Decision 1 "Spoof-channel closure … still required" |
| `set -u` discipline (default expansion on every ledger read) | **Carried**, Decision 5 |
| pwsh ordering (`using namespace` must be first) | **Carried**, Decision 5 delivery layer 1 |
| nushell spike gate (red+green before any parity claim) | **Carried**, Validation |
| Absolute-binary call sites / wrapper shadowing | **Carried**, Decision 5, with the regression test |
| Ledger size cap | **Carried but changed** — 8 KiB → 16 KiB and the ladder reversed, both unmarked. See Q-7 |
| `SHIM_CONTRACT_VERSION` gets its first consumer | **Dropped, consciously** — Decision 6 skips the shim marker with a stated reason and cites `rc_block.rs`'s existing version+hash state machine instead. Correct call (`setup.rs:77` remains declared-unused) |
| `OCX_ENV → __OCX_ENV` (OD-4, dual-read) | **Dropped, consciously**, with the strongest reasoning in the document |
| **bash `PROMPT_COMMAND` ordering; starship / VS Code shell-integration coexistence** | **Dropped silently.** The predecessor named these as a Risk requiring explicit tests, paired with the pwsh wrap. The new ADR keeps only the pwsh half; `PROMPT_COMMAND` survives as a bare mechanism name in Decision 5 with no append-versus-clobber discipline, `starship` appears only as a *thin-dispatcher precedent*, and VS Code appears only as a path-canonicalization citation. Nothing in the Validation matrix covers prompt-hook coexistence on POSIX |
| **Missing-binary probe guard** | **Weakened silently.** The predecessor made it a doctrine amendment with a named mechanism: the hook body "must degrade to a silent no-op when the binary is missing (probe guard, same posture the shims already have)", scoping `adr_idempotent_path_move_to_front.md`'s capture invariant. The new ADR's Amends line still says "capture invariant re-scoped, not repealed" but the body never re-scopes it, and the guard survives only as "a missing binary … degrade[s] to a no-op" in an NFR bullet, with no mechanism and no test |

**Scenario for the silent one.** A bash user with starship (which owns `PROMPT_COMMAND` via its own hook) gets ocx's hook appended, prepended, or overwritten depending on RC ordering. `PROMPT_COMMAND` clobbering is the single most common integration bug in this category, it was named in the predecessor, and it is now untested and unspecified.

## Q-11 — The `$OCX_HOME` sweep carve-out protects a set the same decision proves is empty — unchecked green — CONFIRMED

**Claim attacked.** "**Global-tier carve-out**: `$OCX_HOME` never has a ledger entry (ARCH-1b no-self-link, `registry.rs:321`). Its state key `name_for_path($OCX_HOME)` is **exempt from the sweep** … The global scope needs no consent stamp at all — `$OCX_HOME/ocx.toml` is the user's own file."

**Why it fails.** Those two sentences cancel. If the global scope needs no consent stamp, nothing is ever written to `state/projects/<name_for_path($OCX_HOME)>/`, the directory never exists, and the exemption never fires. The branch's behaviour is identical whether it is implemented correctly, implemented backwards, or not implemented at all — `quality-core.md`'s "a green that cannot be told from 'never ran' is not a check", applied to a guard rather than a test. No test in the Validation list can demonstrate it red.

The invariant it leans on is real (`registry.rs:321` and `clean.rs:170-178` both confirm `$OCX_HOME` is barred from the ledger and re-added implicitly), which is exactly why the carve-out is unnecessary — the *ledger* exemption already exists; a second exemption in a store the global tier never writes to is YAGNI machinery in a security-adjacent sweep.

**Either** delete it, **or** name the future global-tier state that will live there and give it a test that can go red. Leaving it as written means the first person to put global state under `state/projects/` inherits a permanently un-GC'd directory and an exemption no test defends.

## Q-12 — Two behaviours exist only inside the Validation list — CONFIRMED

**Claim attacked.** Validation: "direnv coexistence (detect `DIRENV_DIR`, yield with one info line — explicit non-goal to fight direnv)"; NFR Operability: "verbosity `silent | summary | full`".

**Why it fails.** In the predecessor, direnv-yield was a Consequence/Risk with stated semantics. Here, the *only* statement of what ocx does when direnv is active is a parenthetical inside an acceptance-test bullet. No decision defines what "yield" means: does the hook skip the project scope and keep the global one? Does it revert what it already applied? What happens when `DIRENV_DIR` names a *different* directory than the current project? A test asserting behaviour no decision specifies will be written to match whatever the implementation does, which is the inverse of contract-first.

Verbosity has the same shape with less excuse — three named levels, no surface to select them (Decision 5's flags are `--hook`/`--no-hook`/`--reconcile`; the env vars are `OCX_NO_HOOK`, `OCX_TRUST_*`; `[shell]` carries `hook` and `trust` only), and no default named.

## Decisions that survived the attack

One line each, as requested.

- **Decision 1, carrier placement (env var, not disk).** Survives cleanly. The nested-shell atomicity argument is correct and decisive, the pyenv/conda failure citations are the right ones, and refusing compression on measurement grounds is right (see Q-7 for the cap and ladder, which do not survive).
- **Decision 1, `OCX_ENV` not renamed.** Survives. Three concrete downstream breakages against zero benefit to this ADR is the strongest reasoning in the document, and withdrawing a predecessor decision on that evidence is the correct use of superseding.
- **Decision 2, rejecting D (`.ocx/` in-project stamp).** Survives — a project-writable consent stamp is GHSA-436v-8fw5-4mj8 by construction, correctly scored fatal.
- **Decision 2, rejecting B (promote the ledger entry).** Survives on its own terms: the dual-shape read path and `remove_dir_all` inside the module carrying the SEC-1/TOCTOU guards are real, and the failure mode is data loss. (What does not survive is the claim that C is free of a relocated hazard — Q-4 — and the A-vs-C scoring — Q-5.)
- **Decision 3, list-kind element algebra.** Survives. Element removal commuting with foreign prepends is the one place D1's "structurally impossible" claim is literally true, and `remove_path_element` correctly extends the existing `Shell::escape_value`/`export_path` seam rather than hand-rolling beside it.
- **Decision 3, "keys outside D ∪ L are never read or written".** Survives — this is the direnv whole-env-capture defect removed by construction, and it is the ADR's best structural idea.
- **Decision 5, option C (read config once at shell start).** Survives, including the explicit refusal to promote `self activate` to `Context::try_init` and the refusal to write a bespoke mini-parser. The rejections of A (shim substitution, guarded by an existing test) and B (exported toggle leaking to children) are both grounded in shipped constraints.
- **Decision 5, `OCX_NO_HOOK` boolean over a tri-state.** Survives — six shipped `OCX_NO_*` precedents, and "auto is what unset means" is exactly right.
- **Decision 6, the thin-dispatcher invariant and the three lag surfaces.** Survives, and correcting the predecessor's false "one `self update` later" framing against `shims.rs:461-464` is the best-evidenced claim in the document. Nushell's one-hop lag is honestly scoped.
- **Decision 7, splitting `[shell]` per-key on the digest-pin precedent.** Survives as *structure* — reusing `guard_managed_sigstore_trust`'s home and idiom rather than inventing a rule is right. (The payload it gates does not survive Q-1, and the gated half is deferrable per Q-9.)
- **Decision 8, accepting the silent digest swap.** Survives, and is the most honest section in the ADR: it names the residual, refuses the re-confirm that would train blind confirmation, and points at the signature control instead of building a second weaker one. (Decision 4's grammar then builds a second weaker one anyway — Q-1.)
- **Migration/rollout breakage table.** Survives — the read-path-stays-compatible exception is correctly identified as the single one-way element, and pre-1.0 "breaks just break" is applied without migration prose.
