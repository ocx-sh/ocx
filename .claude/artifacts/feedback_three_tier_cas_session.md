# Session Feedback: Three-Tier CAS Storage Implementation

Captured from a multi-hour architecture + implementation session. These are patterns that went wrong or required repeated correction, and should inform AI configuration updates.

## 1. Agents Don't Read Quality Rules Unless Told

**Problem:** Builder and reviewer agents consistently missed rules from `rust-quality.md` and `code-quality.md` — `pub(crate)` anti-pattern, `&PathBuf` return types, sync `std::fs` in async context, missing use of existing utilities.

**Root cause:** Agent prompts don't automatically include project quality rules. The orchestrator had to manually add "Read `.claude/rules/rust-quality.md` before making changes" to every prompt.

**Fix needed:** Either:
- The builder/reviewer/tester agent definitions (`.claude/agents/worker-*.md`) should mandate reading quality rules as step 1
- Or a hook/rule should inject quality rule references into agent prompts automatically
- Or the swarm-execute skill should include quality rule reading as part of its protocol

## 2. Existing Utilities Ignored — Code Reinvented

**Problem:** The codebase has `utility::fs::DirWalker` for async directory traversal, `PackageDir` for package directory structure, `DIGEST_FILENAME` constant, etc. Agents created manual implementations instead of using them.

**Examples:**
- TagStore used manual `tokio::fs::read_dir` loop → should have used DirWalker
- pull.rs hardcoded `"manifest.json"`, `"resolve.json"` → should have used `PackageDir` accessors
- Stores hardcoded `"digest"` → should have used `DIGEST_FILENAME` constant
- `MAX_WALK_DEPTH` hardcoded `4` → should have derived from `CAS_SHARD_DEPTH`

**Root cause:** Agents don't explore the codebase for existing utilities before writing new code. The "understand first" principle (CLAUDE.md) isn't enforced in agent prompts.

**Fix needed:** Builder agent prompts should include a mandatory "grep for existing implementations before writing new code" step. The architecture-principles rule lists cross-cutting modules — agents should consult it.

## 3. Swarm Workflow Not Followed Automatically

**Problem:** The plan specified contract-first TDD (stub → verify → specify → implement → review), but execution required manual orchestration. The `/swarm-execute` skill exists but has `disable-model-invocation: true`, so the orchestrator had to manually implement each phase.

**What happened:** The orchestrator launched builder agents for stubbing, then testers, then builders again, then reviewers — manually, step by step. This is exactly what `/swarm-execute` automates.

**Root cause:** `disable-model-invocation: true` prevents the orchestrator from invoking the skill. The user had to type `/swarm-execute`. But the plan says "Execute via `/swarm-execute`" — there's a gap between plan intent and execution capability.

**Fix needed:** Either:
- Allow model invocation of swarm-execute (remove `disable-model-invocation: true`)
- Or document in the plan that the orchestrator must manually follow the swarm-execute protocol
- Or create a hook that reminds the orchestrator to follow the protocol when executing plans

## 4. Commits Without User Approval

**Problem:** The orchestrator tried to commit after each implementation without asking. User had to explicitly say "don't commit every time, just make the fixes and only commit after I'm telling you."

**Root cause:** The plan says "Commit → conventional commit message" as the final step, and the swarm-execute protocol includes committing. But the user wants control over when commits happen.

**Fix needed:** The memory file `feedback_no_auto_commit.md` already exists but wasn't consistently followed during plan execution. The swarm-execute skill should defer commit timing to the user.

## 5. Checkpoint vs Conventional Commits

**Problem:** Used `task checkpoint` (amends into single checkpoint commit) instead of proper conventional commits per plan milestone.

**Root cause:** CLAUDE.md says "Use `task checkpoint` to save progress during development" but the user wanted reviewable conventional commits per plan.

**Fix needed:** The plan should explicitly state "conventional commit per plan, not checkpoint" — which it now does. But the orchestrator defaulted to checkpoint before being corrected.

## 6. Over-Engineering Simple Tasks

**Problem:** The TagStore `list_repositories` went through 4 iterations:
1. Sync `std::fs::read_dir` (wrong — blocking)
2. Manual `tokio::fs::read_dir` async BFS (worked but user wanted DirWalker)
3. DirWalker with complex classify + sync fallback (over-engineered, buggy)
4. DirWalker with `collect_and_descend` (correct, simple)

**Root cause:** The orchestrator tried to use the DirWalker but its API didn't support the use case (collect files + descend). Instead of extending the DirWalker first, the orchestrator built increasingly complex workarounds.

**Lesson:** When an existing utility doesn't fit, extend it — don't work around it. The user had to point this out: "just make sure subdirectories are traversed asynchronously. That's it!"

## 7. Reviewer Agents Miss Obvious Issues

**Problem:** The reviewer approved code that:
- Used `pub(crate)` (explicit anti-pattern in rust-quality.md)
- Returned `&PathBuf` instead of `&Path`
- Used sync `std::fs` in tests
- Had duplicate constants (`CAS_ALGORITHMS` vs `SHARD_DIGEST_ALGORITHMS`)
- Hardcoded path strings that should use type accessors

**Root cause:** The reviewer prompt didn't include specific quality rules to check. When the prompt was updated to list key rules explicitly, the reviewer caught more issues.

**Fix needed:** The worker-reviewer agent definition should mandate reading `rust-quality.md` and `code-quality.md`. The OCX quality checklist from those files should be part of every review prompt, not optional context.

## 8. Agent Reports Don't Match Reality

**Problem:** Builder agents reported "all tests pass" and "cargo check clean" but the actual state had compilation errors or test failures. This happened because:
- The agent's final check was correct at the time
- But rust-analyzer diagnostics showed errors from intermediate states
- The orchestrator couldn't distinguish stale diagnostics from real errors

**Lesson:** Always run `cargo check` and `cargo nextest run` directly after receiving agent output before trusting the report. Don't rely on agent self-reporting alone.

## 9. Design Record Drift

**Problem:** Several design decisions from the ADR conversation weren't implemented correctly:
- Digest truncation was missing (full hex in path instead of 32 chars)
- `pub(crate)` used despite being an anti-pattern
- Blob forward-refs not created in pull pipeline despite being in the ADR

**Root cause:** The ADR is a long document. Agents implementing specific plans don't read the full ADR — they get summarized instructions. Details get lost in translation.

**Fix needed:** Each plan's agent prompt should reference specific ADR decision numbers (D1-D11) relevant to that plan, with the key constraints quoted inline.

## 10. Architecture Decisions Need Verification Tests

**Problem:** The GC `orphaned_by_seeds` algorithm was incorrect for the seeds-as-roots case. This was a pre-existing bug made more visible by the refactoring. The reviewer didn't catch it; the user did.

**Lesson:** Architectural invariants should have explicit test cases written from the design record, not derived from the implementation. The specification-first testing approach (write tests from design, not from code) catches these issues.

## Summary: Top Priority AI Config Fixes

1. **Agent definitions must mandate quality rule reading** — add to `worker-builder.md`, `worker-reviewer.md`, `worker-tester.md`
2. **Builder agents must search for existing utilities** before writing new code — grep for patterns, check utility modules
3. **Reviewer prompts must include the quality checklist** from `rust-quality.md` — not as a reference but as items to verify
4. **Plan prompts must quote relevant ADR decisions** inline — don't assume agents will read the full ADR
5. **Swarm-execute protocol should be followable by the orchestrator** without user invocation of the skill
