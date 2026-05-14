# Documentation Review: PR #87 — OCI Referrers Sign + Verify (Slice 1)

**Date:** 2026-05-14  
**Reviewer:** worker-doc-reviewer  
**Merge base:** `1fa82446`  
**Branch:** `feat/oci-referrers-sign-verify`

---

## Summary: Gaps Found

**Triggers matched:** 9  
**Critical gaps:** 1  
**Medium gaps:** 3  
**Accuracy issues:** 1  
**Actionable (total):** 5  
**Deferred:** 3

---

## Trigger Audit

| Source change | Trigger | Doc target | Status |
|---|---|---|---|
| `crates/ocx_cli/src/command/package_sign.rs` (new cmd) | New CLI command | `reference/command-line.md#package-sign` | PASS — section exists, all flags documented |
| `crates/ocx_cli/src/command/verify.rs` (new cmd) | New CLI command | `reference/command-line.md#package-verify` | PASS — section exists, all flags documented |
| New `OCX_IDENTITY_TOKEN` env var | New env var | `reference/environment.md#ocx-identity-token` | PASS — documented with precedence table and security rationale |
| New `OCX_IDENTITY_TOKEN` | `CLAUDE.md` env table | Line 104 of `CLAUDE.md` | PASS — present in env table with "NOT forwarded" note |
| `crates/ocx_lib/src/oci/sign/**`, `verify/**` | New user-visible feature | `user-guide.md#supply-chain` | PASS — use-case-driven section with sign + verify workflow |
| `crates/ocx_lib/src/oci/sign/**` (new sign pipeline) | New in-depth page | `in-depth/signing.md` | PASS — new page covers trust root, referrers cache, bundle storage, identity matching, slice boundary |
| New features | `CHANGELOG.md` | `[Unreleased] Added` | PASS — entries for sign, verify, OCX_IDENTITY_TOKEN, exit codes 82/83 |
| `website/src/docs/in-depth/signing.md` (new file) | VitePress sidebar | `.vitepress/config.mts` | PASS — `{ text: "Signing", link: "/docs/in-depth/signing" }` present at line 89 |
| `crates/ocx_lib/src/cli/exit_code.rs` (new codes 82/83) | Exit code reference | `reference/command-line.md` sign + verify exit tables | PASS — codes 82 and 83 in both tables |

---

## Critical Gaps (user-visible behavior undocumented)

- [ ] **`crates/ocx_cli/src/command/package_sign.rs:129-132` → `.claude/rules/subsystem-cli-commands.md` Command Summary table** — `package sign` and `package verify` are absent from the Command Summary table. All 27 other commands appear in this table. AI agents consulting this quick-reference will not discover the new commands, leading to missed context during planning and review cycles. The commands are user-visible and CI-script-targetable. Remediation: add two rows — `package sign ID` (purpose: "Publish Sigstore keyless signature via OCI Referrers", key flags: `-p`, `--identity-token-file`, `--identity-token-stdin`, `--no-tty`, `--no-cache`) and `package verify ID` (purpose: "Verify Sigstore keyless signature via OCI Referrers", key flags: `-p`, `--certificate-identity`, `--certificate-oidc-issuer`, `--no-cache`).

---

## Medium Gaps (edge cases, internal changes)

- [ ] **`crates/ocx_cli/src/command/sigstore_url.rs:9-10` → `website/src/docs/in-depth/signing.md`** — The loopback carve-out in `validate_sigstore_url` (HTTP allowed on `127.0.0.0/8`, `::1`, `localhost`) enables pointing `--fulcio-url` / `--rekor-url` at a local fake-sigstore stack for development and integration testing. The in-depth page mentions the local `fake_fulcio` trust-root injection path (`TrustRoot::load_from_pem`) but says nothing about the URL validation policy or the loopback exception. Publishers who want to test the signing pipeline against a private staging sigstore will find no guidance. Remediation: add a brief `:::tip Testing against a local Sigstore stack` callout after the Slice Boundary section documenting that `--fulcio-url http://127.0.0.1:<PORT>/...` is accepted and how to pair it with the PEM trust-root option.

- [ ] **`crates/ocx_lib/src/oci/sign/error.rs:130` → `website/src/docs/reference/command-line.md#package-sign` exit codes** — `OidcPreCheckFailed` maps to exit 77 (`PermissionDenied`) alongside `OfflineSignRefused` and `IdentityTokenFilePermissive`. The doc exit-code table row for code 77 reads "Offline mode active; OIDC pre-check failed; token file has permissive permissions" (`command-line.md:1594`). This is technically correct but does not explain what an "OIDC pre-check failed" condition is (e.g. Windows rejecting `--identity-token-file`). Scripts switching on exit 77 cannot distinguish the offline case from a token-file rejection. Remediation: split exit 77 into two table rows or add a sub-bullet explaining the three sub-conditions that produce it — offline, permissive file mode, and pre-check failure (Windows platform rejection of `--identity-token-file`).

- [ ] **`crates/ocx_cli/src/command/verify.rs:38-65` → `website/src/docs/in-depth/signing.md#identity-matching`** — The identity matching section says "Wildcard and regex matching are planned for Slice 2 — the flags are intentionally named for the eventual match-policy expansion." The reference doc at `command-line.md:1272` similarly says "Exact match only in Slice 1." However, neither page documents *how* the exact-match comparison is performed for the GitHub Actions workflow URL form (e.g. whether trailing ref components are normalized). Operators who need to write the exact-match string for a workflow SAN have to guess the format. Remediation: add one concrete example for each supported identity form (email and workflow URL) to the Identity Matching section, showing the full string that must be supplied to `--certificate-identity`.

---

## Accuracy Issues (existing docs now incorrect)

- [ ] **`website/src/docs/reference/command-line.md:1549`** — "Signing requires network access — `--offline` is rejected with exit 77." The exit code is correct per `sign/error.rs:130-131` (`OfflineSignRefused → PermissionDenied = 77`). However, the exit-code table at `command-line.md:1594` groups offline rejection with OIDC pre-check failure and permissive token-file permissions under one exit-77 row, making the rejection reason ambiguous to script consumers. The doc statement at line 1549 is accurate but incomplete — the exit-codes table should reinforce it with a dedicated row. This is a presentation gap rather than a factual error, but it risks script authors writing `if [ $? -eq 77 ]; then retry` for an offline failure that will never succeed on retry. Severity: Medium. Remediation: split or annotate the exit 77 table row as described in the Medium gap above.

---

## Deferred (style judgment, no clear factual gap)

- [ ] **`website/src/docs/in-depth/signing.md` — Diátaxis type: reference content mixed into In-Depth narrative.** The Referrers Capability Cache section (`#referrers-cache`, lines 19-36) lists the exact JSON schema of the cache file (four fields with types). This is reference-level detail that fits better in `reference/environment.md` (next to the `$OCX_HOME/state/referrers/` path description) or in a `reference/file-formats.md` page. Keeping it in the in-depth narrative is not wrong but breaks Diátaxis separation. Deferred: requires architectural decision on where file-format reference lives.

- [ ] **`website/src/docs/user-guide.md#supply-chain` — Missing Slice 1 preview callout.** The user guide presents the sign and verify workflow as if fully functional. The reference pages carry `:::warning Preview / not yet fully implemented` callouts. The user guide has no corresponding caveat for the slice-1 limitation. A reader following the user guide before the reference page will attempt to sign a package and get an `unimplemented!()` panic. The "Learn more" tip at line 477-480 links to the reference pages but the slice limitation is not surfaced in the guide itself. Deferred: whether to add a preview callout to the user guide is a product communication decision.

- [ ] **`website/src/docs/authoring/building-pushing.md#signing-after-push` — No example of signing multiple platforms.** The "Signing after push" section shows `ocx package sign -p linux/amd64 my/cmake:3.28` but the `--platform` flag is required per source and pushing multi-platform packages requires signing each platform separately. No guidance is given on how to loop over platforms. This is a completeness gap, not an inaccuracy. Deferred: the authoring section is intentionally minimal ("Sign a release" user guide and reference page are the primary surfaces); adding a loop example may be appropriate in a later authoring polish pass.

---

## Checklist Summary

| Check | Result |
|---|---|
| All new CLI flags/commands in `reference/command-line.md` | PASS |
| `OCX_IDENTITY_TOKEN` in `reference/environment.md` | PASS |
| `OCX_IDENTITY_TOKEN` in `CLAUDE.md` env table | PASS |
| `signing.md` linked in VitePress sidebar | PASS |
| `user-guide.md#supply-chain` use-case-driven (not file-structure-first) | PASS |
| `in-depth/signing.md` covers trust root, referrers cache, bundle format, identity, slice boundary | PASS |
| Internal cross-refs resolve (user-guide → signing.md, cmd-package-sign, cmd-package-verify) | PASS |
| External links have hyperlinks (Sigstore, Fulcio, Rekor, cosign, OCI spec) | PASS |
| Link syntax uses reference-style (not inline) | PASS |
| CHANGELOG.md entries present for sign, verify, env var, exit codes | PASS |
| `subsystem-cli-commands.md` Command Summary table updated | **FAIL — Critical gap** |
| Exit-77 sub-condition disambiguation | **FAIL — Medium gap** |
| Loopback URL carve-out documented for local testing | **FAIL — Medium gap** |
| Identity-match exact-string format examples | **FAIL — Medium gap** |

---

## Citations

| Finding | Source |
|---|---|
| sign/verify absent from CLI commands table | `.claude/rules/subsystem-cli-commands.md:70-78` |
| Loopback carve-out in validate_sigstore_url | `crates/ocx_cli/src/command/sigstore_url.rs:9-10` |
| TrustRoot::load_from_pem in in-depth page | `website/src/docs/in-depth/signing.md:15` |
| OidcPreCheckFailed → PermissionDenied | `crates/ocx_lib/src/oci/sign/error.rs:130-132` |
| Exit-77 doc row | `website/src/docs/reference/command-line.md:1594` |
| Exact-match Slice 1 note | `website/src/docs/in-depth/signing.md:67-68` |
| Sidebar signing entry | `website/.vitepress/config.mts:89` |
| OCX_IDENTITY_TOKEN env section | `website/src/docs/reference/environment.md:150-168` |
| CHANGELOG sign/verify entries | `CHANGELOG.md:16-19` |
