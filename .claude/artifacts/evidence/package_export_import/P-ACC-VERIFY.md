# P-ACC-VERIFY — acceptance for auto-verify during `ocx package import`

Command: `cd test && OCX_COMMAND=/home/mherwig/.herdr/worktrees/ocx/uno/.agents/worktrees/a0-ref-build/target/debug/ocx uv run pytest tests/test_package_import_verify.py -q -p no:randomly`
Result: 2 failed, 0 passed (Sigstore stack up; sign step succeeded in both).

- `test_import_verifies_from_carried_referrers` · red: `assert exported.returncode == 0` in `_signed_export` (test_package_import_verify.py:72), exit 101, `not yet implemented: P-EXPORT: export` · the panic is the stubbed `package export`, a setup step of this scenario, not `import` itself.
- `test_import_enforces_policy_identity_from_carried_referrers` · red: same line, same panic · same reason.

Limit: against A0 both reach only the export step, so the import assertions (exit 0, zero requests, exit 79 `no_signatures_found`, exit 77) are not yet shown red. Export is a carried-content prerequisite that cannot be produced without the stub being implemented; they are first exercised once P-EXPORT lands.
