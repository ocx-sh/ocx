# ADR: Windows `.cmd` Argument Injection (BatBadBut / CVE-2024-24576 Class)

## Metadata

**Status:** Accepted (interim) — supersedes §"Tension 3 — Windows Shell Targets + Argument Escaping" in `adr_package_entry_points.md` with respect to the security surface of the `.cmd` template.
**Date:** 2026-04-27
**Deciders:** Michael Herwig (PR #64 round-2 review, Codex B1 finding)
**GitHub Issue:** N/A (surfaced in PR review, tracked here)
**Related ADR:** [`adr_package_entry_points.md`](./adr_package_entry_points.md) — parent design for generated launchers
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path (`product-tech-strategy.md`) — no new language or framework; existing Rust/`.cmd` approach retained
- [x] Deviation in future work (compiled shim) deferred and justified in §Future Work
**Domain Tags:** security, package-manager, cli
**Supersedes:** §"Tension 3 — Windows Shell Targets + Argument Escaping" (security sub-section only) in `adr_package_entry_points.md`
**Superseded By:** —

---

## Context

OCX generates `.cmd` batch launchers on Windows at install time. The current template
(as of the `feat/package-entry-points` branch, code at
`/home/mherwig/dev/ocx/crates/ocx_lib/src/package_manager/entrypoints.rs:179–188`) is:

```bat
@ECHO off
SETLOCAL
ocx exec "file://{pkg_root}" -- "%~n0" %*
```

`%*` forwards caller-supplied arguments via a second pass of `cmd.exe`'s parser.
This is the BatBadBut class of vulnerability (CVE-2024-24576 / April 2024 Flatt Security
disclosure). A caller who passes `"& whoami"` as an argument can cause `cmd.exe` to
interpret `& whoami` as a second shell command, executing arbitrary code in the caller's
session.

**What is already mitigated.** Publisher-controlled surfaces — the baked `pkg_root` and
the entry-point name used in `%~n0` — are guarded by `LauncherSafeString`
(`entrypoints.rs:40–60`), which rejects `%`, `"`, `'`, `\n`, `\r`, and `\0` at launcher
generation time. These characters cannot appear in the baked template string, so the
`file://` URI and the `%~n0` fragment are not injection vectors.

**What is not mitigated.** `%*` is expanded at invocation time from caller-supplied
arguments. `LauncherSafeString` cannot reach caller argv — those are not known at
generate time. The current `SETLOCAL` (without `DisableDelayedExpansion`) also leaves
open a narrower vector: if the Windows registry key
`HKCU\Software\Microsoft\Command Processor\DelayedExpansion` is set to `1`, delayed
expansion is active globally and `!VAR!` syntax in `%*` is expanded before the command
runs, even inside what would otherwise be quoted regions.

Full technical analysis including CVE timeline, cmd.exe parsing layers, and crate
survey is in the research artifact:
[`research_batbatbut_mitigations.md`](./research_batbatbut_mitigations.md).

---

## Decision Drivers

1. **Security baseline** — CVE-2024-24576 has a CVSS 10.0 rating; OCX's own
   `LauncherSafeString` demonstrates awareness of the problem class. Shipping
   no mitigation on the `%*` surface would be inconsistent.
2. **Backend-automation user audience** — OCX is a backend tool used by CI
   systems, Bazel rules, and devcontainer features (see `product-context.md`).
   In typical use, callers are automation scripts that control the argument strings.
   This narrows the realistic attacker surface but does not eliminate it; a CI
   pipeline passing user-supplied strings (issue titles, branch names) to an OCX
   launcher is a real threat model.
3. **Cross-compile feasibility** — A compiled `.exe` shim is the definitive fix
   but requires a cross-compilation target (`x86_64-pc-windows-gnu`) added to CI,
   a new binary artifact, distribution packaging changes, and a shim file format
   contract. These are non-trivial and out of scope for the current PR.
4. **Release-cycle cost** — The interim mitigation is a one-line change plus a
   test golden update. The deferred compiled shim is a separate feature with its
   own ADR scope.
5. **Ecosystem precedent** — npm `cmd-shim` v8.0.0 (October 2025) still ships
   unescaped `%*`. The Node.js fix went into the runtime, not the template. This
   does not excuse OCX from doing better, but it establishes that the industry
   has not converged on a pure-template solution.

---

## Threat Model

| Surface | Exploitable? | Mitigated? | Mechanism |
|---|---|---|---|
| `pkg_root` baked in template | No | Yes | `LauncherSafeString` rejects `%`, `"`, `\n`, `\r`, `\0`, `'` at generation time |
| Entry-point name (`%~n0`) | No | Yes | `EntrypointName` slug regex rejects `/`, `\`, `..`, non-slug chars |
| Caller argv via `%*` | **Yes** | **No (partially after interim fix)** | cmd.exe re-parses metacharacters in `%*` expansion |
| Delayed expansion via `!VAR!` in `%*` | **Yes** (registry opt-in) | **Closed by interim fix** | `DisableDelayedExpansion` in `SETLOCAL` neutralises `!VAR!` expansion |
| `ocx exec` internal | No | N/A | Rust binary receives clean argv via `CommandLineToArgvW`; not cmd.exe-parsed |
| `target` field from metadata | No | N/A | Not baked into launcher; resolved by `ocx exec` from `metadata.json` at invocation time |

**Residual risk after interim fix.** Caller-supplied arguments containing `( ) % ^ " < > & |`
outside double-quoted regions are still re-parsed by `cmd.exe` through `%*`. Any caller
that passes user-controlled strings — for example a CI pipeline interpolating a branch
name or a build label — into an OCX launcher invocation without shell-quoting is
vulnerable to command injection. `DisableDelayedExpansion` closes only the `!VAR!`
vector; it does not close the broader `%*` re-parse surface.

---

## Considered Options

### Option 1 — `%*` escape loop in pure `.cmd` template

**Description.** Implement a pure-batch loop that iterates over each argument, escaping
`%` → `%%`, cmd.exe metachars with `^`, and re-quoting for `CommandLineToArgvW`.

| Pros | Cons |
|---|---|
| No new binary artifact | Rust std team concluded the general case is intractable |
| Self-contained — no external dependency | Two parsing layers (cmd.exe + CommandLineToArgvW) have irreconcilable quoting rules for adversarial input |
| Matches the "fix in template" mental model | Implementation would be 50–100 lines of batch, difficult to test exhaustively |

**Verdict:** Rejected. The Rust std team's assessment (accompanying CVE-2024-24576 fix
in Rust 1.77.2) is that pure-batch escaping is not feasible for general argument strings.
This is not an edge case; it is the documented failure mode that makes BatBadBut
exploitable despite years of awareness.

### Option 2 — Rust-side static escape at generation time

**Description.** Pre-escape known-static strings at launcher generation time via a Rust
function, rejecting or transforming any character that cmd.exe treats as a metachar.

| Pros | Cons |
|---|---|
| Publisher surface already protected this way | Caller argv cannot be pre-escaped — they are not known at generation time |
| Rust-level validation is reliable | Does not address the `%*` expansion surface at all |

**Verdict:** Rejected for this problem. `LauncherSafeString` already implements this for
publisher-controlled fields. It is the right tool for static surfaces; `%*` is dynamic.

### Option 3 — Compiled `.exe` shim (definitive fix, deferred)

**Description.** A small Rust binary reads its own filename via `GetModuleFileNameW`,
reads `pkg_root` from a sibling `.shim` text file, and calls `CreateProcessW` directly
with a properly assembled argv. Bypasses `cmd.exe` entirely.

| Pros | Cons |
|---|---|
| Eliminates `cmd.exe` re-parse surface entirely | Requires cross-compilation toolchain addition (`x86_64-pc-windows-gnu`) |
| Handles Ctrl+C/Break forwarding correctly | New binary artifact to distribute, sign, and test |
| 22 KB binary feasible (see hermetic-launcher prior art) | Sibling `.shim` file format contract to design and maintain |
| Cross-compiles from Linux CI — no Windows-only build step | Out of scope for current PR |

**Verdict:** Correct long-term solution. Deferred to follow-on work. See §Future Work.

### Option 4 — PowerShell `.ps1` shim as replacement

**Description.** Replace the `.cmd` launcher with a `.ps1` launcher. PowerShell parses
args with its own rules, not cmd.exe's.

| Pros | Cons |
|---|---|
| PowerShell argument parser does not re-expand `%*`-style tokens | PowerShell has its own injection vectors (`$(...)` expansion, operator injection) |
| | `.ps1` blocked by default execution policy on many Windows configurations |
| | cmd.exe callers still invoke the `.cmd` launcher; PowerShell is not a substitute |
| | Already deferred in `adr_package_entry_points.md` (Tension 3) for architectural reasons |

**Verdict:** Rejected. Does not substitute for a `.cmd` fix. Architectural deferral from
the parent ADR stands.

### Option 5 — Runtime gating (disallow `.cmd` spawn without `shell:true`)

**Description.** Follow the Node.js CVE-2024-27980 approach: detect when `ocx exec` is
invoked from a `.cmd` launcher and apply runtime validation of arguments.

| Pros | Cons |
|---|---|
| Centralised fix in `ocx exec` rather than in every launcher template | Complex heuristic to detect "called from a `.cmd` launcher" reliably |
| | Would require `ocx exec` to change behaviour based on its caller's identity — fragile and out of scope for the exec command contract |
| | Does not protect callers who bypass `ocx exec` and invoke the launcher via a shell |

**Verdict:** Rejected. The runtime-gating approach works for platform runtimes that
control the spawn path; OCX does not control how callers invoke launchers.

### Option 6 — Documentation only

**Description.** Document the `%*` risk in the user guide and leave the template
unchanged. Require callers to shell-quote arguments.

| Pros | Cons |
|---|---|
| Zero implementation cost | Sole reliance on caller discipline is unacceptable for a CVE-class issue |
| | OCX launchers are machine-invoked; callers are automation scripts not reading documentation |

**Verdict:** Rejected as a sole mitigation. Documentation of residual risk is required
*in addition to* a technical mitigation, not instead of one.

### Option 7 — Interim hardening + threat-model documentation + deferred compiled shim (CHOSEN)

**Description.** One-line template change: `SETLOCAL` → `SETLOCAL DisableDelayedExpansion`.
Document the residual `%*` risk explicitly. Track compiled `.exe` shim as the deferred
definitive fix.

| Pros | Cons |
|---|---|
| Closes the registry-level `!`-expansion vector immediately | Residual `%*` caller-argv injection remains |
| One-line change; minimal blast radius | Does not close the general cmd.exe metachar re-parse surface |
| Explicit threat-model documentation sets expectations for callers | Compiled shim deferred — residual risk period is open-ended |
| Sets up the compiled shim as a clean follow-on without any compat break | |
| Consistent with YAGNI: defer the compiled shim until architecture is ready | |

**Verdict:** Chosen. See §Decision Outcome.

---

## Decision Outcome

**Chosen Option:** Option 7 — interim hardening (`SETLOCAL DisableDelayedExpansion`) +
threat-model documentation + deferred compiled shim.

**Rationale.** The compiled shim (Option 3) is the only complete solution, but it
introduces infrastructure complexity — a new cross-compile target, a new binary artifact,
and a sibling `.shim` file format contract — that is out of scope for this PR. Options 1
and 2 do not address the actual attack surface. Options 4, 5, and 6 are either
architecturally wrong or insufficient as sole mitigations.

The one-line interim change is meaningful: it eliminates the narrower but real
delayed-expansion vector (`!VAR!`) that is active when the Windows registry key
`HKCU\Software\Microsoft\Command Processor\DelayedExpansion` is set. This vector can
be triggered without any special privileges by any process that sets that registry key,
making it a realistic attacker path even in semi-trusted environments.

**Honest residual risk statement.** After the interim fix, the `%*` caller-argv
re-parse surface remains open. A CI pipeline that interpolates untrusted strings
(user-supplied branch names, issue titles, build labels) into arguments passed to an
OCX `.cmd` launcher is exploitable. Mitigating factors: OCX's primary audience is
automation where argument strings originate from trusted build configuration; and the
industry norm (npm cmd-shim v8.0.0) is to accept `%*` unescaped and rely on runtime
or caller discipline. OCX documents this explicitly rather than relying silently on
ecosystem convention.

---

## Consequences

**Positive:**
- Closes the `!VAR!` delayed-expansion vector that a process with registry write
  access to the current user's `Command Processor` key could activate.
- Documents the residual `%*` caller-argv risk explicitly in the threat model and
  user guide, giving callers actionable information rather than silent exposure.
- Produces a clean, small diff reviewable in isolation, unblocking the PR.
- Sets up the compiled `.exe` shim as a well-defined follow-on with no compat
  break: the `.cmd` launcher remains present as a fallback, and the shim can be
  added alongside it without regenerating any installed launcher.

**Negative:**
- Caller-argv injection via `%*` remains until the compiled shim lands. Any
  caller who does not control all argument values is exposed.
- CI tooling that invokes OCX launchers with user-supplied argument strings without
  shell-quoting (e.g., `cmake $BRANCH_NAME --version`) could be exploited if
  `$BRANCH_NAME` is attacker-controlled.

**Risks:**
- **Industry-norm acceptance gap.** The precedent that npm cmd-shim ships unescaped
  `%*` may cause security reviewers to dismiss the residual risk. OCX should not cite
  this as justification; it is cited only as ecosystem context.
- **Compiled shim deferred indefinitely.** If the follow-on shim work never lands,
  the "interim" hardening becomes permanent. The implementation plan below includes a
  GitHub issue to prevent this.

---

## Future Work — Compiled `ocx-shim.exe` (deferred)

The definitive fix bypasses `cmd.exe` entirely by using a compiled Rust binary:

- **Reads own filename** via `GetModuleFileNameW` to determine the entry-point name.
- **Reads `pkg_root`** from a sibling `.shim` text file (Scoop pattern — simpler than
  binary patching; one line: the absolute package-root path).
- **Calls `CreateProcessW`** directly with
  `ocx exec "file://<pkg_root>" -- "<name>" <argv...>`. Rust std `Command` issues
  `CreateProcessW` correctly for non-batch targets, with `CommandLineToArgvW`-compatible
  quoting. No `cmd.exe` layer.
- **Forwards Ctrl+C / Break** via `SetConsoleCtrlHandler`.
- **Size target**: <50 KB (`opt-level = "z"`, `lto = true`, `panic = "abort"`,
  `strip = "symbols"`).
- **Cross-compile from Linux CI**: `x86_64-pc-windows-gnu` target via MinGW. No
  Windows-only build step required.

Prior art and evaluation: hermetic-launcher (Rust, ~22 KB, MIT, active, Bazel-oriented)
is the closest reference. Full comparison table in
[`research_batbatbut_mitigations.md`](./research_batbatbut_mitigations.md) §Option 3.

When the shim lands, the install pipeline generates `<name>.exe` + `<name>.shim` instead
of (or alongside) `<name>.cmd`. The `.cmd` file can be retained as a fallback for
environments where `.exe` execution is blocked, making the transition additive rather
than a hard cutover.

---

## Implementation Plan

### Immediate (this PR)

1. **Code change** — In
   `/home/mherwig/dev/ocx/crates/ocx_lib/src/package_manager/entrypoints.rs`, function
   `windows_launcher_body` (line 185), change:

   ```bat
   SETLOCAL
   ```
   to:
   ```bat
   SETLOCAL DisableDelayedExpansion
   ```

   The resulting template body:
   ```bat
   @ECHO off
   SETLOCAL DisableDelayedExpansion
   ocx exec "file://{pkg_root}" -- "%~n0" %*
   ```

2. **Test update** — Update the golden assertion in
   `windows_launcher_body_byte_exact_match_adr_form` (entrypoints.rs:392–401) to expect
   `SETLOCAL DisableDelayedExpansion`. Add a negative assertion:

   ```rust
   assert!(
       !body.contains("EnableDelayedExpansion"),
       "template must not enable delayed expansion: {body}"
   );
   ```

3. **CHANGELOG** — Add a `### Security` entry:

   ```
   - Windows `.cmd` launchers now use `SETLOCAL DisableDelayedExpansion` to
     neutralise the `!VAR!` delayed-expansion vector. Residual `%*` caller-argv
     injection risk documented in user guide.
   ```

4. **User-guide callout** — In `website/src/docs/user-guide.md`, under the Windows
   launcher section, add a callout block:

   > **Residual Windows risk.** OCX launchers forward caller arguments via `%*`.
   > If your automation passes user-controlled strings as arguments to an OCX launcher
   > without shell-quoting, those strings are re-parsed by `cmd.exe` and may contain
   > metacharacters (`&`, `|`, `^`, `<`, `>`) that execute as shell commands.
   > Shell-quote all arguments before passing them to OCX launchers, or wait for the
   > compiled `.exe` shim (tracked in GitHub issue #TBD) which bypasses `cmd.exe`
   > entirely.

### Deferred (follow-on PR)

5. **GitHub issue** — Open a tracking issue for `ocx-shim.exe`. Reference this ADR,
   hermetic-launcher, and the cross-compile toolchain requirement. Link from the user
   guide callout above.

6. **CI toolchain** — Add `x86_64-pc-windows-gnu` to the cross-compile matrix.

7. **Shim binary** — Implement `crates/ocx_shim/` with the design in §Future Work.
   Write a dedicated ADR for the shim binary's contract (sibling `.shim` file format,
   argv forwarding, Ctrl+C handling, size budget, code signing).

8. **Install pipeline** — Extend `generate()` in `entrypoints.rs` to write `<name>.exe`
   + `<name>.shim` alongside (or instead of) `<name>.cmd`.

---

## Cross-References

- **Parent ADR**: [`adr_package_entry_points.md`](./adr_package_entry_points.md) — §"Tension 3 — Windows Shell Targets + Argument Escaping" establishes the `.cmd`-only v1 decision; this ADR supersedes the security sub-section of that decision.
- **Research artifact**: [`research_batbatbut_mitigations.md`](./research_batbatbut_mitigations.md) — CVE timeline, cmd.exe parsing layers, escape-loop intractability analysis, shim comparison table, npm cmd-shim behavior post-CVE.
- **Code**: `/home/mherwig/dev/ocx/crates/ocx_lib/src/package_manager/entrypoints.rs` — `windows_launcher_body` (line 179), `LauncherSafeString` (line 40), golden test (line 392).
- **CVE-2024-24576** — [Rust advisory GHSA-q455-m56c-85mh](https://github.com/rust-lang/rust/security/advisories/GHSA-q455-m56c-85mh); fixed in Rust 1.77.2.
- **CVE-2024-43402** — [GHSA-2xg3-7mm6-98jj](https://github.com/rust-lang/rust/security/advisories/GHSA-2xg3-7mm6-98jj) — Rust 1.81.0 bypass of the CVE-2024-24576 fix via trailing whitespace or period in the executable path, causing `cmd.exe` to re-parse the argument as a batch invocation. The stdlib fix in Rust 1.81.0 patches `std::process::Command` when spawning `.bat`/`.cmd` targets. **OCX relevance:** OCX launchers are statically generated text files; they do not invoke `std::process::Command` on `.bat` or `.cmd` targets at launcher-generation time. The stdlib fix is therefore irrelevant to OCX's attack surface. Cross-ref recorded here for security reviewers auditing the BatBadBut class. Per max-tier review finding SOTA-1.
- **CVE-2024-27980** — Node.js response: disallow `.bat`/`.cmd` direct spawn without `shell: true`.
- **hermetic-launcher**: [hermeticbuild/hermetic-launcher](https://github.com/hermeticbuild/hermetic-launcher) — Rust shim prior art.
- **BatBadBut disclosure**: [Flatt Security, RyotaK, 2024-04-09](https://flatt.tech/research/posts/batbadbut-you-cant-securely-execute-commands-on-windows/).

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-27 | Architect worker (Sonnet 4.6, PR #64 round-2) | Initial draft — interim hardening + threat model + deferred shim |
| 2026-04-29 | worker-doc-writer (Sonnet 4.6) | Expanded CVE-2024-43402 cross-reference to clarify OCX-specific irrelevance: OCX generates static text files, not `Command` invocations on `.bat`, so Rust 1.81.0 stdlib fix does not apply to OCX's attack surface. Recorded for security reviewer context (SOTA-1). |
