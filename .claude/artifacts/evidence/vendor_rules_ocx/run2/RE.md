# RE evidence (rules_ocx e2e, red first) — scratch record for the orchestrator

Files (rules_ocx worktree, uncommitted): e2e/rbe/tests/test_package_lock.py (new, 29 rows / 30 cases),
e2e/rbe/tests/conftest.py (LockRig publisher over seed.Seeder, minimal workspaces, fact readers, fault wrapper, update helpers),
e2e/rbe/tests/test_caches_facts.py (:96 key `package|dep_tool`; reproducible test gains `tag` case),
e2e/rbe/tests/test_remote_exec.py (test_tag_locked_tool_runs_remotely).

Collection: `uv run --project . pytest --collect-only -q tests/` = 63 (52 before).

Live red run (observed), rig compose project run2-re (nginx+zot+bb storage/frontend/scheduler, ports 18600/18602),
seeded with the first-run pin ocx 0.6.8-pr784+20261126125712 (sha256 2cc82924...e288aa), Bazel 9.2.0 via ocx exec,
--ignore_all_rc_files, scratch output root: log live-all.log.
34 failed / 34 run (test_package_lock.py all 30 cases; test_caches_facts non_root + reproducible x3).
Every failure block carries `extensions.bzl:270 ... unsupported vendor manifest schema_version 1: this rules_ocx reads 2`.
Not run live: test_remote_exec.py (no Buildbarn/NativeLink workers up); offline leg of test_offline_fresh_tag_names_the_fix (red came
from its online first step). Rig torn down (`down -v`), 0 containers left.
Reds by mutation and greens: pipeline IR (after D1).
