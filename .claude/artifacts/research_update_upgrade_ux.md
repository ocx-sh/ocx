# Research: update / upgrade UX prior art

**Date:** 2026-10-05 · **For:** [`plan_update_family.md`](./plan_update_family.md) — ocx-sh/ocx#548, #310, #590
**Axis:** package-manager UX (self-update policy, drift notices, bumping a declared constraint)

## Self-update and update notices

| Tool | Setting / env | Default | Notes |
|---|---|---|---|
| [rustup](https://rust-lang.github.io/rustup/basics.html) | `rustup set auto-self-update enable\|disable\|check-only` (settings.toml); `--no-self-update` | enable | Fires only inside an explicit `rustup update`, never as a background check. The new binary takes effect on the next run. |
| [mise](https://mise.jdx.dev/configuration/settings.html) | `auto_update` (`MISE_AUTO_UPDATE`), `auto_update_check_duration`, `disable_update_warning` | off, 7d | Skipped in CI, offline and non-interactive sessions. **Global-only**, so a project config cannot opt a user into replacing their binary. Re-execs the original command on the new binary. |
| [gh](https://raw.githubusercontent.com/cli/cli/trunk/internal/update/update.go) | `GH_NO_UPDATE_NOTIFIER`, `GH_NO_EXTENSION_UPDATE_NOTIFIER` | notify, 24h | Only when not CI and stdout+stderr are TTYs. The state file caches the last check time and latest release. |
| Homebrew | `HOMEBREW_AUTO_UPDATE_SECS` (86400), `HOMEBREW_NO_AUTO_UPDATE` | 24h | Interval in bare seconds. |
| [uv](https://github.com/astral-sh/uv/blob/main/crates/uv/src/commands/self_update.rs) | `uv self update [--dry-run]` | none | No background update. Refuses unless installed by its own installer, and fails under `--offline`. |
| proto, volta, asdf | — | — | No comparable interval settings found (unverified). |

## Bumping a declared constraint

| Tool | Default | Opt-in to cross a major |
|---|---|---|
| [proto outdated --update](https://moonrepo.dev/docs/proto/commands/outdated) | Newest within the range | `--latest` |
| [cargo upgrade](https://github.com/killercup/cargo-edit) | Compatible only | `--incompatible` |
| [mise upgrade --bump](https://mise.jdx.dev/cli/upgrade.html) | Keeps precision (`20` → `22`) | `--bump` is itself the crossing |
| [npm-check-updates](https://github.com/raineorshine/npm-check-updates) | Crosses majors | `--target minor\|patch` restricts |
| [Renovate Docker](https://docs.renovatebot.com/docker/) | Keeps precision (`1.1` → `1.2`) and suffixes; majors on | `docker:disableMajor`, `separateMajorMinor` |

The common shape: a report with two columns, newest-compatible vs latest (proto "Newest/Latest", npm "Wanted/Latest"). Majors are shown but not applied by default.

## Recommendations adopted by the plan

1. **`ocx upgrade` stays within the current major, at the current precision; `--major` crosses.** The report always shows the newest tag beyond the major as information. (`--major` rather than cargo's `--incompatible`: OCX tags are not semver ranges.)
2. **Renovate: no ocx-side datasource.** OCX tags live in plain OCI registries, so Renovate's `docker` datasource plus a regex custom manager over `ocx.toml` already handles precision and majors. A custom datasource needs an HTTP endpoint, not CLI output. A docs snippet (with an `ocx lock` post-upgrade task) is a follow-up, not this plan.
3. **`self = apply` must not be settable from a repository.** It matches mise's global-only rule. `ocx.toml` contributes a project tier to `Config`, so `[update]` is stripped there like `[shell]` and `[records]`.
4. **Apply takes effect on the next run, without a re-exec** (rustup's behaviour). It goes through the existing self-update hand-off and is skipped for an ocx that ocx did not install (uv's receipt rule ≈ our `Skipped(Bootstrap)`).
5. **Keep a bare-number interval as seconds and `0` as always.** A 1d default matches gh and Homebrew.
6. **Document `manual` as "no background check; explicit commands still work".** `OCX_NO_UPDATE_CHECK` remains the global off.
7. **`update` moves the lock and `upgrade` moves the tag** (`cargo update` / `cargo upgrade`). Both help texts say so.
