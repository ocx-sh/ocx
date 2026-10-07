# Security review: ADR + system design, OCX interface contract

**Reviewer:** security focus, hex-architect xhigh panel (opus). **Repo:** `/home/mherwig/dev/ocx` @ `34337a6a2`.
**Under review:** `adr_ocx_interface_contract.md`, `system_design_ocx_interface_contract.md`.
**Verdict:** Needs Work. 3 Block, 9 Warn, 6 Suggest, 1 Deferred. All Blocks are fixes to the spec text, not reasons to reject Option D.

Evidence: I read the code cited for every finding at the HEAD above. Nothing was built or run, so no claim below rests on a test result.

---

## Block

### B1. "Testing" is a naming rule, but the risk is an ungated read (CWE-489)
**Anchor:** SD §3.4 registry unit tests ("prefix per visibility"); SD §6, Threats row 1; ADR § Out-of-scope security findings ("its registry declaration is `Testing`, which the naming lint rejects without the prefix").
**Evidence:** the defect in `OCX_TEST_FAULT` is that it is **not cfg-gated** (`crates/ocx_project/src/mutation.rs:256-260,283`). Its spelling is secondary. Every correctly spelled hook is gated by hand: `__OCX_TESTING_RENDER_FAULT` (`ocx_package_manager/src/tasks/render_toolchain.rs:1692`), `__OCX_TESTING_HELPER_TIMEOUT_MS` (`ocx_oci/src/auth/store.rs:33-40`), and `__OCX_TESTING_FORGE_BASE_URL` (`ocx_announce/src/forge/github.rs:853`, `gitlab.rs:1469`). The last one redirects forge API calls, and the announce tokens go to those calls. `release_feature_set_excludes_testing_seams` (`ocx_test_support/tests/workspace_structure.rs:676`) only proves that the release feature set has `__testing` off. It cannot prove that a read is gated. Suppose the hook had been spelled `__OCX_TESTING_FAULT`: the proposed prefix lint passes and the hook still ships. Phase 0 also rewrites every one of these reads into `vars::X.get()`, and nothing in the design makes that rewrite keep the `#[cfg]`.
**Fix:** make `env_vars!` emit each `Testing` entry under `#[cfg(any(test, feature = "__testing"))]`, and leave those entries out of `ALL` when that cfg is off. An ungated read then fails to compile in a release build. Add a verification row: seed an ungated `.get()` of a Testing var, then `cargo build --release -p ocx --locked` (the `taskfiles/release.taskfile.yml:67` lane) must go red. The Bazel lane cannot serve as this proof, because `//crates/ocx_cli:ocx_cli` always builds `__testing` (`crates/ocx_cli/BUILD.bazel:57`). Rewrite SD §6 row 1 to name the cfg gate as the mitigation and drop the prefix lint from that row.

### B2. Generated argv allows option injection (CWE-88)
**Anchor:** SD §3.11, Rust backend output ("argv built as a vector"); SD §6 "Argument injection through an SDK".
**Evidence:** spawning without a shell prevents CWE-78 only. It does not stop an untrusted positional value from being parsed as a flag. Concrete case: `ocx exec` flattens `--env NAME[=TYPE[:SEP]]=VALUE` (`command/exec.rs:59-60`, `options/env_override.rs`), and its `packages` positional does not allow hyphen values (`exec.rs:85`). A generated call `exec(packages=[untrusted], command=[...])` given the identifier `--env=LD_PRELOAD=/tmp/x.so` therefore builds `["exec","--env=LD_PRELOAD=/tmp/x.so","--","tool"]`. clap parses that as the `--env` flag. `is_reserved_ocx_key` reserves only `OCX_*`, so the child process runs with an attacker-chosen `LD_PRELOAD`, which is code execution. The same shape applies to `--records`, `--output` and `--platform` on other commands. `cli.json` (`ArgSpec`) does not record `allow_hyphen_values`, `value_terminator` or `last`, so the generator cannot place positionals safely.
**Fix:** spec the following in §3.11 and enforce them in the IR loader. (1) Every flag value is emitted as one token, `--long=value`. (2) A positional value that starts with `-` is refused client-side with a typed error, unless the grammar marks that positional `allow_hyphen_values` (as for `exec`'s `command`). Identifier, Platform and Digest values can never legitimately start with `-`. (3) `ArgSpec` gains `allow_hyphen_values`, `value_terminator` and `last`. (4) V9/V10 get a red case: an identifier `--env=X=1` must not reach argv.

### B3. The ADR authorizes silent hard breaks for renamed env vars, and some fail open
**Anchor:** ADR § Flag renames, the batched window, last paragraph ("…others are hard breaks"); SD §5 IC-13 (polarity `OCX_X`/`OCX_NO_X`), IC-06 (env values snake_case).
**Evidence:** a removed flag fails loudly. A renamed env var is silently ignored, because nothing reads the old name. Several vars are hardening-direction, so a rename quietly drops the protection the user set: `OCX_FROZEN` (`ocx_config/src/env.rs:23-25`, lock enforcement), `OCX_OFFLINE` (`:21-22`, network egress), `OCX_NO_CONSENT` (`:38-42`, consent stamping), `OCX_NO_CONFIG_REFRESH` (`:82-85`, managed-tier refresh), and `OCX_RECORDS_DIR` (`:86-88`, audit records turn off). A polarity flip carried by an alias inverts the meaning. Renaming a value fails open as well, because `flag()` turns an invalid value into the default with only a warning (`ocx_util/src/env.rs:151-163`). This contradicts the ADR's own C1 test: "loud (not silent) at runtime".
**Fix:** add `retired: &'static [&'static str]` to `EnvVar`. At startup, scan the environment once for retired names. Inside the window, warn on stderr. After the removal release, exit with config error 78 and never ignore the name. No polarity change rides an alias: the new name is new, and the old name is retired, which means an error. Env value renames accept the old spelling with a warning, the same mechanism the ADR already plans for clap choice values. Amend the ADR paragraph to read "others are retired names (error), never silently dropped".

---

## Warn

### W1. The `Secret` attribute only drives `CREDENTIAL_KEYS`, and it mixes secrecy with propagation (CWE-532)
**Anchor:** SD §3.4 `enum Secret { No, Inherited, Scrubbed }`; ADR NFR Security ("`secret` entries are never logged").
**Evidence:** the seam logs key names only on its UTF-8 path (`ocx_util/src/env.rs:143-146`), but `flag()` logs the parse error (`:160`), and `BooleanStringError` echoes the raw value (`ocx_util/src/boolean_string.rs:10-11`). The new typed getters (`Integer`, `Choice`, `Json`) add more parse-error paths and nothing says they must not echo values. Other secret consumers are separate hand lists: the `ocx_script` deny list (`ocx_script/src/ocx_module.rs:30-55`) covers `OCX_AUTH_*` only, so a script's `ocx.env("OCX_ANNOUNCE_TOKEN")` succeeds, because that token is deliberately left out of `CREDENTIAL_KEYS` (`ocx_config/src/env.rs:129-133`). Foreign secrets are read too: ambient OIDC `ACTIONS_ID_TOKEN_*` (`ocx_sign/src/sign/oidc_ambient_inline.rs`) and `CI_JOB_TOKEN`/`GITHUB_TOKEN` (forge credentials). `Inherited` reads as "not secret, just forwarded".
**Fix:** split the attribute into `secret: bool` (never formatted anywhere) and `child: Scrub | Inherit | Forward`. For `secret` entries, `get()` returns a newtype with no `Display` and a redacting `Debug`, so the compiler blocks echoing the value, and parse errors for those entries omit it. Pattern entries (`OCX_AUTH_{REGISTRY}_TOKEN`) match by prefix and suffix. Derive `CREDENTIAL_KEYS`, the `ocx_script` deny set and ocx-mirror's `OCX_VARS` exclusion from the registry. Add a registry test: every name matching `(?i)(token|password|secret|passphrase|_key)$` declares `secret = true` unless it is on a reviewed allowlist. Foreign entries carry the attribute too.

### W2. The error document's `context` is open-ended and no redaction is specified (CWE-209 / CWE-532)
**Anchor:** SD §3.3 `context: ErrorContext // known keys optional, open for additions`; ADR tension 3; SD §3.8 "Additive (reported, never red)".
**Evidence:** `message` is the whole `{err:#}` chain (`render_error_envelope`, `crates/ocx_cli/src/error_envelope.rs:106-110`). Redaction happens per construction site, with two duplicated `redact_url` (`ocx_index/src/file_transport.rs:40`, `ocx_index/src/ocx_index.rs:53`). No central guarantee exists. A new `context` key is additive, so the compat gate passes it without a ledger entry: a key carrying a URL with userinfo would ship unseen. The `error.detail` slugs are fine, because they are `&'static str`.
**Fix:** add lint L14: every `context` property is a `$ref` to a vocabulary type (`PackageRef`, `Digest`, `RegistryHost`, `AbsolutePath`). Free strings and raw URLs are not allowed. If a URL is ever needed, it uses a `RedactedUrl` vocabulary type that strips userinfo when constructed. Add a poisoned-secret test: set `OCX_AUTH_*_TOKEN`, `OCX_IDENTITY_TOKEN`, an `HTTPS_PROXY=http://u:p@…` and a userinfo mirror URL, trigger failures under `--format json`, and assert the poison bytes appear nowhere on stdout or stderr. The `poisoned-ambient-read` env in `crates/ocx_cli/BUILD.bazel:232-234` is the precedent. `machine-interface.md` should state that `message` and `context` may contain absolute local paths and hosts, and never credentials.

### W3. The SDK handshake is presented as a trust check, and binary resolution is unspecified
**Anchor:** SD §3.12 `Ocx::from_path()`; SD §6 row 4.
**Evidence:** the handshake checks compatibility only. Any binary on `PATH` that prints the expected `version` JSON passes it. §6 lists the handshake as a mitigation against "an untrusted … binary". ocx-mirror today reads `OCX_BINARY_PIN` raw, then falls back to `PATH` (ocx-mirror `ocx_cli.rs:31-38`).
**Fix:** state in §6 that binary integrity is the caller's job and the handshake is not a trust boundary. `from_path()` resolves once to an absolute path and stores it; the SDK never re-resolves per call. Prefer `OCX_BINARY_PIN`, which ocx sets for its children (`ocx_config/src/env.rs:13-15`). On Windows, every language backend resolves only `ocx.exe` and refuses `.cmd`/`.bat`; this is the BatBadBut class, CVE-2024-24576, and Python and Node `which` honour PATHEXT. Recommend `Ocx::new(abs_path)` in CI.

### W4. `cli.json` defaults capture the generator host's environment
**Anchor:** SD §3.6 `ArgSpec.default`.
**Evidence:** `default_value_t = ocx_util::env::flag(...)` at `crates/ocx_cli/src/app/context_options.rs:42,55,64,77,88` runs when the `Command` is built. The walker therefore writes the generating host's `OCX_GLOBAL`, `OCX_REMOTE`, `OCX_OFFLINE`, `OCX_FROZEN` and `OCX_QUIET` into the golden and into every published SDK. Today these are booleans, so no secret leaks. The pattern still bakes host environment into a published artifact and makes the golden depend on the host.
**Fix:** in phase 0, replace env-derived defaults with literal defaults and resolve the env after parsing through the registry's `flag` link. The walker runs under `overrides::lock().hermetic(..)`, and lint C07 asserts that every default is literal.

### W5. The `Plumbing` prefix `^_{1,2}OCX_` reaches outside the reserved-key gate
**Anchor:** SD §3.4 registry tests; SD §5 IC-14.
**Evidence:** `is_reserved_ocx_key` reserves `OCX_` and `__OCX_` only (`ocx_util/src/env.rs:198-202`). Package metadata (`ocx_package/src/metadata/validation.rs:202`, `metadata/env/apply.rs:335`) and `--env` (`ocx_cli/src/options/env_override.rs:133`) rely on it. A future `_OCX_*` plumbing var could therefore be set by a published package. None is read today: `_OCX_APPLIED` is removed (`environment.md:55-57`).
**Fix:** restrict `Plumbing` to `^__OCX_`. Add a registry test that `is_reserved_ocx_key(name)` holds for every non-Foreign entry. Keep the gate prefix-based and never derive it from the registry's exact names, because that would make undeclared `OCX_*` names settable.

### W6. Typed `EnvValue` must not flatten per-site validation
**Anchor:** SD §3.4 `EnvValue`, `flag_or`.
**Evidence:** several reads are deliberately bespoke. `OCX_TOOLCHAIN_PINNED` is tri-state and "never read via `flag`" (`ocx_config/src/env.rs:102-105`). `OCX_ENV` decodes fail-closed while `OCX_PATCHES` is lenient (`:62-69`). `OCX_EXTRA_CA_CERTS` takes a path **or** inline PEM (`:113`), which `Path` mis-models. `OCX_INSECURE_REGISTRIES` is a `host[:port]` list. `flag()` turns an invalid hardening bool into the default with only a warning.
**Fix:** add `EnvValue::{PathOrPem, HostList, TriBool}` and `on_invalid: Default | Error`; hardening bools declare `Error`. Migration rule: a site with bespoke validation keeps it, and `get_raw()` exists for it. Add one test per security-relevant var (insecure registries, extra CA, consent, signing key, `OCX_ENV`) pinning the current invalid-value behaviour.

### W7. Secrets passed on stdin have no channel in the grammar or the SDK (CWE-214)
**Anchor:** SD §3.6 `ValueSpec`; §3.11.
**Evidence:** `--password-stdin` (`command/login.rs:29`) and `--identity-token-stdin` (`package_sign.rs:82`, `package_attest.rs:101`) export as plain `Switch`. A generated API exposes a bool with no way to feed stdin, which pushes callers toward argv. `OCX_KEY_PASSWORD` is "never a flag, since argv is visible host-wide" (`ocx_config/src/env.rs:121`).
**Fix:** add `ArgSpec.stdin_secret: bool`. The generator emits a stdin bytes parameter for such flags, never an argv value. Lint C08: no flag whose long name matches `token|password|secret|passphrase` takes a `String` value.

### W8. The baseline `RELEASE` ref is unvalidated and the gate can be laundered
**Anchor:** SD §3.8 Baseline (`git show $(cat RELEASE):…`).
**Evidence:** `RELEASE` can name any ref, or a commit with forged goldens. A PR could edit `baseline/*` and `RELEASE` together and pass a break with no ledger entry. An option-shaped value (`--output=…`) becomes a `git show` option.
**Fix:** the T0 lint asserts that `RELEASE` matches `^v\d+\.\d+\.\d+$`, resolves it with `git rev-parse --verify --end-of-options refs/tags/<v>^{commit}`, and checks that it equals the newest `v*` tag that is an ancestor of `HEAD`. Add CODEOWNERS on `crates/ocx_schema/contract/**`.

### W9. Open enums fail open on security-decision fields
**Anchor:** ADR § Enum policy (supersedes the signing ADR's bump rule).
**Evidence:** "Consumers MUST tolerate unknown values" plus `Unknown(String)` means `match s { Failed => …, _ => ok }` treats a new value as success. Fields where this applies: `SweptStatus` (`api/data/sweep.rs:18-40`, which per-row consumers such as mirror read) and `AttestationOutcome` (`api/data/push.rs:167-169`). The superseded signing-ADR rule (a bump on a new `kind`) was the conservative default.
**Fix:** add to the policy text: "a consumer deciding success MUST treat an unknown status/outcome as failure; the exit code is authoritative". The generator emits fail-closed predicates (`is_success()` returns false on `Unknown`) for every `status` / `*Outcome` `$def`.

---

## Suggest

- **S1. Signing for the generator distribution.** Anchor: SD §3.11 Distribution. Sign `ocx.sh/ocx/sdkgen` keyless under the release-workflow identity. SDK repos pin it by digest (`ocx.lock`) and carry a `[[trust.policy]]` for that identity so the default auto-verify enforces it. Publish `ocx-sdk` through crates.io trusted publishing (OIDC), with no long-lived token.
- **S2. Pinning for the oasdiff spike.** Anchor: ADR tension 4 / phase-2 go/no-go. Pin the exact version plus its checksum (Go module sum or release-asset sha256), run it outside any job holding secrets, and record the version and digest in the spike artifact, so the go/no-go evidence is reproducible. Keeping it out of `ocx.toml` is correct.
- **S3. A semantic list for the phase-1 never-null rewrite.** Anchor: SD §5 IC-04. `VerificationReport.signatures` is "absent while empty, never `[]`: absence does not mean discovery looked and found none" (`api/data/verification.rs`). A mechanical "known empty → `[]`" collapses that distinction. List the verification, signature, attestation, sweep and push-attestation roots for a mandatory `semantic` review in phase 1.
- **S4. SDK hygiene.** Anchor: SD §3.12. The generated `Ocx` `Debug` redacts `env` values for manifest entries with `secret = true`, and stdout and stderr reads are bounded (a cap with a typed error).
- **S5. The ban cannot see every read path.** Anchor: SD §3.5. Clippy cannot see reads inside dependencies (`SSL_CERT_FILE`/`SSL_CERT_DIR` set CA trust and the proxy vars set routing, both documented at `environment.md:1114-1158`). Re-audit these with `cargo vendor` + grep on each dependency bump. Also assert in `workspace_structure.rs` that clap's `env` feature stays off (`Cargo.toml:112`), because `#[arg(env = …)]` would bypass the registry entirely.
- **S6. `OCX_CEILING_PATH` semantics.** See the verdict below.

---

## Verdicts on the two discovery findings

### `OCX_TEST_FAULT`, `OCX_TEST_FAULT_RELEASE_FILE`: **Warn (Medium), CWE-489**
- **Exploitability:** it needs control of the ocx process environment. `is_reserved_ocx_key` stops package metadata, project `[env]` and `--env` from setting any `OCX_*` name, and I found no remote route to set it. Anyone who already controls the env has far stronger levers (`OCX_CONFIG`, `OCX_INSECURE_REGISTRIES`, `OCX_EXTRA_CA_CERTS`).
- **Impact:** `pause_before_manifest_write` without a release file polls every 50 ms **with no timeout**, after the new `ocx.lock` is already on disk (`ocx_project/src/mutation.rs:281-291`, reached from `commit` at `:141-161`). That hangs `ocx add`/`lock`, and killing the process leaves the lock ahead of the manifest; the crash-recovery path exists (`test/tests/test_project_crash_recovery.py`). The `*_lock_*` stages force a failure and a rollback. The realistic trigger is a developer who exported the variable while debugging.
- **Handling:** agree with the ADR: file a separate bug-fix issue now. Gate it with `#[cfg(any(test, feature = "__testing"))]`, rename it `__OCX_TESTING_FAULT{,_RELEASE_FILE}`, add an `ocx_project/__testing` feature and its forward entry (`ocx_cli/Cargo.toml:25`, policed by `testing_feature_forward_list_matches_grep`), and give the pause a bounded timeout. Acceptance tests keep working because Bazel's `ocx` always has `__testing` (`crates/ocx_cli/BUILD.bazel:57`); update `test_project_crash_recovery.py:140,218,269` and `test_lock.py:1447`. B1 is what stops this class from recurring.

### `OCX_CEILING_PATH`: **Suggest (Low), not a vulnerability**
- It is read through the seam (`ocx_config/src/loader.rs:469`) and can only **narrow** discovery. The walk already stops at `.git` and rejects symlinked candidates (`loader.rs:567-597`), so someone with env control gains nothing beyond what `OCX_NO_PROJECT` already gives. The only use outside unit tests is `test/tests/test_project_config_home_walk.py:44`.
- I agree with open question 2: make it **Public**. Before documenting it, fix or document one trap. The comparison is lexical (`current == ceiling` at `loader.rs:593-594`, with the ceiling run through `lexical_normalize` and never canonicalized). A ceiling spelled through a symlink, such as macOS `/tmp` → `/private/tmp`, never matches, so the bound silently does nothing. Users will expect `GIT_CEILING_DIRECTORIES`-style containment. Canonicalize the ceiling (keep the lexical form if it does not exist) or document "resolved path, single entry, inclusive". Put this in a separate small issue now; the phase-0 coverage test forces the docs either way.

---

## Deferred

- **D1. `ocx_script`'s `ocx.env` credential deny is cosmetic.** `ocx.env` hides `OCX_AUTH_*` (`ocx_script/src/ocx_module.rs:44-55`), but `ocx.run` children get `env_clear().envs(base_env)` (`:81-83`). That base env is the inherited process env minus `CREDENTIAL_KEYS` only, so `OCX_AUTH_*` and `OCX_ANNOUNCE_TOKEN` reach any program the script runs. This is pre-existing and outside this design's scope. Reason: human judgment is needed on whether a `--script` script is a trust boundary. If it is, scrub the child env from the registry's `secret` set (W1); if it is not, delete the deny so the code stops implying a boundary.
