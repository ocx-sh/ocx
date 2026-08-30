# WP-4 — `env://` key reference scheme (issue #389)

Branch `hex/issue-sweep--wp4`, base `52eecc15`.
Commits: `f0420258` (feature + docs), `d66995d1` (acceptance tests).

## Contracts

- **C-030 — DONE.** `Scheme::Env` added; `SPELLINGS` is `["file", "env", "awskms", …]`
  (7 entries, table order preserved); `Scheme::parse` gained `"env"`;
  `is_implemented()` is `matches!(self, Self::File | Self::Env)`. Both non-exhaustive
  sites carry a named test and both were proved red — see the proof table.
  `ALL_SCHEMES` is `[Scheme; 7]`, `only_the_file_backend_is_implemented` became
  `only_the_file_and_env_backends_are_implemented` and derives its expectation from
  `matches!(scheme, File | Env)` rather than from `is_implemented` itself.
- **C-031 — DONE.** `KeyRef::as_env_var() -> Option<&str>` sits beside `as_path()` and
  returns the variable *name*. Both consumers gained an `Env` branch:
  `build_signer` (`tasks/sign.rs:322-328`) via `PemKeyBackend::open_env`, and
  `compile_key_reference` (`trust.rs`) via `key_ref::read_key_env`. No new crypto —
  `from_encrypted_pem` already took raw bytes.
- **C-032 — DONE.** `KeyBackendKind::Env` → wire value `"env"` (serde `snake_case`,
  pinned by `key_backend_kind_slug_matches_scheme_spelling` and
  `the_display_slug_is_the_serde_slug`). `impl From<Scheme>` forced the arm as
  predicted. The changelog-bearing subject is `f0420258`.
  `FileKeyBackend` was renamed `PemKeyBackend` and carries the source as a `kind`
  field: `kind()` returning a hardcoded `File` would have reported `file` for an
  env-sourced signature. `from_encrypted_pem` now takes the kind explicitly rather
  than defaulting — a wire value a caller does not know must not be guessed.
- **C-033 — DONE.** `key_ref::read_key_env` is one reader for both halves: unset or
  empty → `KeyEnvError::Unset` (exit 74, the code a missing key *file* gets), over
  `MAX_KEY_PEM_BYTES` → `KeyEnvError::TooLarge` (exit 65, the code an over-cap file
  gets). Every message names the variable. `OCX_KEY_PASSWORD` is untouched and both
  coexist. Routed through `crate::env::var`, which is what makes it testable without
  `unsafe` or a mutated process environment.
- **C-034 — DONE, premise corrected.** `options/key.rs:22-26` never listed `env`
  among the rejected schemes — the stale-help risk ran the other way: an implemented
  scheme absent from the help. The help now documents `env://VAR` and names
  `OCX_SIGNING_KEY` as the scrubbed name. `the_help_describes_every_scheme_the_way_the_parser_treats_it`
  loops over `Scheme::SPELLINGS`, requiring an implemented scheme to appear as
  `<s>://` and **not** as a bare backticked name, and an unimplemented one to appear
  as a bare backticked name. It catches both directions and a scheme that flips
  status later.
- **C-035 — DONE.** `.claude/rules/subsystem-cli.md` gained
  "Secret-bearing values: the `env://` convention" directly under the credential
  exemption table, framed as the general shape for secret-bearing flags. It states
  the consequence: an operator-chosen name **is inherited by plugins and generated
  entrypoint launchers**, nothing warns, and that is the operator's knowing choice.
- **C-036 — DONE.** `keys::OCX_SIGNING_KEY` added and joined to `CREDENTIAL_KEYS`.
  Membership is asserted by name in `the_conventional_signing_key_variable_is_a_credential`,
  because the existing scrub test iterates the list and would stay green with the
  entry removed — it would simply test one variable fewer.
- **C-036a — DONE.** `CREDENTIAL_KEYS`'s doc comment is now the single documented
  surface: a **membership rule** ("if holding the string authenticates you, it is a
  credential" — with the counter-examples), a three-edit **adding-one checklist**
  (const + `subsystem-cli.md` row + `environment.md` entry), a **members table**
  carrying each entry's purpose and read site, and the "not forwarding is not not
  leaking" rationale that previously lived only in `subsystem-cli.md`.
- **C-036b — DONE.** `website/src/docs/reference/environment.md` gained
  `### OCX_SIGNING_KEY` with a worked example and a warning block spelling out that
  any other variable name is inherited. All three credential entries now carry the
  same sentence, in the same words: **"Never forwarded to child processes."**
  (`OCX_IDENTITY_TOKEN`'s older "NOT forwarded to subprocess children" wording was
  normalised and extended to name the plugin case.) The `subsystem-cli.md` exemption
  table gained the matching `OCX_SIGNING_KEY` row.

## Scenarios

- **S-009 — DONE.** `test_sign_with_an_env_held_key_reports_the_env_backend`.
  Signs the golden cosign key passed through `OCX_SIGNING_KEY`; asserts
  `key_backend == "env"`, `signer == "env"`, and that `public_key_hint` matches the
  bundle's — a hint that moved with the reference would mean the source reached the
  key material.
- **S-010 — DONE.** `test_sign_with_an_unset_env_key_names_the_variable`. Exit 74,
  the variable named in `error.message`, and no filesystem wording.
- **S-025 — DONE.** `test_plugin_never_inherits_a_credential_variable`. Dispatches
  `ocx-credprobe` with all three credentials plus one ordinary variable set;
  asserts the three are `<absent>` and the ordinary one is `inherited`. The control
  is what makes the absences evidence — a plugin that never launched would report
  everything absent.

## Red/green proofs

Every mutation was verified to have landed before the run and to have been restored
after it (the harness asserts both and aborts otherwise). Green baseline: `task
rust:verify --force` exit **0**, 6437 tests run, 6437 passed, 8 skipped.

| # | Site mutated | Mutation | Test | Red exit |
|---|---|---|---|---|
| 1 | `Scheme::parse` `_ => None` wildcard | deleted `"env" => Some(Self::Env),` | `the_env_scheme_token_is_recognised_by_the_grammar` | 101 |
| 2 | `Scheme::is_implemented` `matches!` | `File \| Env` → `File` | `an_env_reference_is_implemented_not_merely_recognised` | 101 |
| 3 | same | same | `only_the_file_and_env_backends_are_implemented` | 101 |
| 4 | same | same | `options::key::…::an_env_reference_parses_through_the_library_grammar` | 101 |
| 5 | `From<Scheme>` | `Scheme::Env => Self::File` | `an_env_scheme_reports_the_env_key_backend` | 101 |
| 6 | `read_key_env` cap | `if value.len() > CAP` → `if false` | `read_key_env_bounds_the_value_at_the_shared_cap` | 101 |
| 7 | `read_key_env` empty check | deleted `.filter(\|v\| !v.is_empty())` | `read_key_env_refuses_an_unset_or_empty_variable` | 101 |
| 8 | `open_env` error class | `Io(NotFound)` → `MalformedKey` | `opening_an_unset_env_key_is_an_io_error_naming_the_variable` | 101 |
| 9 | `PemKeyBackend::kind()` | `self.kind` → `KeyBackendKind::File` | `the_reported_backend_follows_the_source_not_the_material` | 101 |
| 10 | `CREDENTIAL_KEYS` | dropped `OCX_SIGNING_KEY` | `the_conventional_signing_key_variable_is_a_credential` | 101 |
| 11 | `compile_key_reference` env branch | `if let …` → `if false && let …` | `an_env_key_reference_compiles_from_the_variable` | 101 |
| 12 | `--key` help text | removed `` `env://VAR` `` | `the_help_describes_every_scheme_the_way_the_parser_treats_it` | 101 |
| 13 | `--key` help text | moved `` `env` `` into the rejected list | same test | 101 |

Acceptance proofs (release rebuild per mutation, both builds exit 0):

| Mutation | Result |
|---|---|
| `build_signer`'s `else if let Some(variable) = key.as_env_var()` branch deleted | S-009 red (`assert 85 == 0`), S-010 red (`assert 85 == 74`) — exit 85 is `UnsupportedKeyBackend`, exactly the silent-miss the plan named. **S-025 still passed**, so the mutation reds only what it should. |
| `CREDENTIAL_KEYS` loses `OCX_SIGNING_KEY` | S-025 red — the failure prints the raw PEM reaching the plugin (`assert '-----BEGIN E…VATE KEY-----' == '<absent>'`). **S-009 and S-010 still passed.** |

Green restored after each: rebuilt from restored source, `test_sign.py` +
`test_plugin_dispatch.py` = **49 passed, 1 xfailed**. Final gate re-run after the
clippy fixes: **exit 0**.

Ruff: `test_sign.py` had 24 pre-existing `PLW1510` findings before my change and has
24 after (the same lines) — my additions carry explicit `check=False`.
`test_plugin_dispatch.py` is clean. Verified against `git show HEAD:…` baseline.

`task claude:verify --force` exit 0 after the `subsystem-cli.md` edit (69 passed,
3 skipped, offline link check clean). Doc anchors: `#ocx-signing-key` resolves;
lychee `--include-fragments` reports zero errors naming it (the six other findings
are pre-existing and unrelated).

## Out-of-scope work done, and why

**Four website files beyond `environment.md`.** They describe the `--key` grammar and
would have shipped saying a bare path or `file://` is all there is. No other WP owns
them (checked against the parallelization table). One clause each:
`reference/command-line.md` (the `sign` and the `verify` `--key` rows, plus a link
definition), `in-depth/signing.md`, `reference/configuration.md` (the `key` field of a
`[[trust.policy]]` signer). `in-depth/cosign-parity.md` was left alone — its KMS bullet
is still accurate.

**`trust.rs::a_key_reference_naming_an_unimplemented_backend_never_becomes_a_key_ref`.**
Outside the declared `compile_key_reference:1063-1067` range, but it is that function's
own contract test and its premise — "every `KeyRef` that exists is a file reference" —
is what C-031 falsifies. It failed on first run. Rewritten to assert **exactly one**
accessor answers for any parseable reference, which is the invariant the fall-through
arm actually depends on and which now also covers a future third accessor.

## Deferred / findings

1. **`OCX_ANNOUNCE_TOKEN` is a bearer credential and is not on `CREDENTIAL_KEYS`.**
   It is a forge personal access token, documented as such in `environment.md`, and it
   satisfies C-036a's membership rule verbatim — so today it reaches every dispatched
   `ocx-<plugin>`. **Not fixed here**: `ocx-mirror` announces from a plugin process and
   plausibly relies on inheriting it, so adding it is a behavioural decision about a
   downstream repo, not a refactor. Needs an owner call.
2. **`OCX_AUTH_<REGISTRY>_TOKEN` cannot go on a fixed list at all** — it is a name
   *pattern*. This is structurally the same hole `env://` has (an unknown name cannot
   be scrubbed) and the same hole the `env://` convention documents rather than closes.
   If #1 is taken up, the fix for both is prefix-based scrubbing, which is a design
   change, not a list entry.
3. **`c96b23dd`'s `key_password()` still reads `std::env::var` directly**, not the
   `crate::env::var` seam that `read_key_env` uses. Left alone — changing it is
   behaviour-neutral but touches the documented "one env-reading entry point" comment
   and buys nothing for WP-4. It is why `open_env`'s *success* path has no unit test
   (a developer with `OCX_KEY_PASSWORD` exported would red it); S-009 covers it
   end to end instead.

## Environment note

Mid-session the harness twice reported the primary working directory moving
(`…/isw-wp7`, then `/home/mherwig/dev/ocx`, then `…/ocx/test`). Every command in this
work package used an absolute path into `.agents/worktrees/isw-wp4`; the worktree,
branch and commits are intact and were re-verified after each notice.
