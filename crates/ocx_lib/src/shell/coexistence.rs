// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! direnv / mise coexistence: does another per-prompt hook already own this
//! directory's environment?
//!
//! Detection half of WP-4 (C-049, A-37). WP-11 owns the behavioural half:
//! narrow `desired` to the global scope, revert the project scope, and print
//! one info line per observed tool.
//!
//! **The yield signal is the other tool's live session state, never a file on
//! disk** (C-020, C-049) — an `.envrc`, a `mise.toml` or a `.tool-versions`
//! checked into a repo where the tool is not installed, not hooked, or not
//! active in *this* shell must not suppress ocx activation: a config file is
//! evidence of someone else's workflow, not of a live hook that will set the
//! env at this prompt. Yielding on file presence would leave the project
//! silently managed by nobody.

use std::path::Path;

use serde::Serialize;

// Third-party sentinel names. Not `crate::env::keys` — those are ocx's own
// vars; these belong to direnv and mise and are never written by ocx.
const DIRENV_DIR: &str = "DIRENV_DIR";
const MISE_SHELL: &str = "MISE_SHELL";
const MISE_ORIG_PATH: &str = "__MISE_ORIG_PATH";

/// A coexisting per-prompt environment manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    /// Yielded on `DIRENV_DIR` **naming the resolved project's canonical
    /// directory**. A `DIRENV_DIR` naming a *different* directory is treated as
    /// absent — direnv is active for some ancestor, not for this project.
    Direnv,
    /// Yielded on `MISE_SHELL` **or** `__MISE_ORIG_PATH` being present.
    Mise,
}

/// One live-session observation: which tool, and the signal that proved it.
///
/// `signal` is what `ocx shell state` renders, because a user staring at an
/// `.envrc` will guess the wrong cause (C-050 reason 4) — it names the variable
/// observed and, for direnv, the directory it named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    /// The tool observed live in this shell.
    pub tool: Tool,
    /// The env-var evidence, rendered for a human.
    pub signal: String,
}

/// The typed yield verdict (C-049).
///
/// An empty `observed` means no yield: reconcile normally. A non-empty one
/// means apply the **global** scope only, revert any project scope already
/// applied, and print **one info line per observed tool**.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Yield {
    /// Every tool observed live, in detection order.
    pub observed: Vec<Observation>,
}

/// Detect which coexisting tools are live for `project_dir` (C-049).
///
/// A-37 — the two checks are **independent `if`s, never an `elif` chain**: ocx
/// yields on a matching `DIRENV_DIR` **or** on `MISE_SHELL` /
/// `__MISE_ORIG_PATH`, regardless of the other's state. With both sentinels set
/// and matching, both observations appear. Red state: an `elif` between the two
/// checks silently suppresses the second tool's line.
///
/// `project_dir` is the canonical directory the CWD walk resolved — direnv's
/// arm compares against it, mise's arm does not use it.
pub fn detect(project_dir: &Path) -> Yield {
    let mut observed = Vec::new();

    // direnv arm. C-020: this reads one env var and compares strings — no
    // stat, no shelling out to `direnv status`. A DIRENV_DIR naming some
    // other (e.g. ancestor) directory is direnv managing a different
    // project, not this one (S-020) — left unobserved, not partially matched.
    //
    // direnv exports the value as `-` followed by the absolute directory — the
    // dash is direnv's own marker, not part of the path (`DIRENV_DIR=-/home/u/p`,
    // verified against direnv 2.35.0). Comparing the raw value matches nothing a
    // real direnv ever sets, so the prefix is stripped before the compare and the
    // raw spelling is kept for `signal`, which is what the user sees in their own
    // environment.
    if let Some(raw) = crate::env::var(DIRENV_DIR)
        && Path::new(raw.strip_prefix('-').unwrap_or(raw.as_str())) == project_dir
    {
        observed.push(Observation {
            tool: Tool::Direnv,
            signal: format!("{DIRENV_DIR}={raw}"),
        });
    }

    // mise arm — deliberately a separate `if`, not `else if`/`elif` off the
    // direnv arm above (A-37: independent, both fire when both are live).
    // MISE_SHELL is mise's primary per-session sentinel; __MISE_ORIG_PATH
    // covers a shell where only the PATH-restore half of the hook ran.
    if let Some(value) = crate::env::var(MISE_SHELL) {
        observed.push(Observation {
            tool: Tool::Mise,
            signal: format!("{MISE_SHELL}={value}"),
        });
    } else if let Some(value) = crate::env::var(MISE_ORIG_PATH) {
        observed.push(Observation {
            tool: Tool::Mise,
            signal: format!("{MISE_ORIG_PATH}={value}"),
        });
    }

    Yield { observed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    /// S-017, C-049 — `DIRENV_DIR` matching the resolved project yields,
    /// naming the variable and the directory in `signal`.
    ///
    /// The fixture is the `-`-prefixed spelling **real direnv exports**, which
    /// is the whole point: an earlier bare-path fixture made this test pass
    /// against a value no direnv ever sets, so the yield was green here and
    /// dead in the field. The bare form is accepted too — being tolerant costs
    /// nothing — and `signal` renders whatever the shell actually carried.
    #[test]
    fn detect_yields_direnv_when_dir_matches_project_s017() {
        let env = test::env::lock();
        let project = tempfile::TempDir::new().expect("tempdir");
        let project_dir = project.path();
        let utf8 = project_dir.to_str().expect("utf8 tempdir path");
        env.remove(MISE_SHELL);
        env.remove(MISE_ORIG_PATH);

        for raw in [format!("-{utf8}"), utf8.to_owned()] {
            env.set(DIRENV_DIR, &raw);

            let verdict = detect(project_dir);

            assert_eq!(
                verdict.observed,
                vec![Observation {
                    tool: Tool::Direnv,
                    signal: format!("{DIRENV_DIR}={raw}"),
                }],
                "DIRENV_DIR={raw:?} must be observed as a live direnv for this project"
            );
        }
    }

    /// S-020 — the `-` is stripped, not treated as a wildcard: a
    /// `-`-prefixed `DIRENV_DIR` naming a **different** directory still does
    /// not yield. Without this, `strip_prefix` could degrade into "any
    /// `-`-prefixed value matches" and the S-017 row above would not notice.
    #[test]
    fn detect_ignores_a_dash_prefixed_dir_naming_another_project_s020() {
        let env = test::env::lock();
        let project = tempfile::TempDir::new().expect("tempdir");
        let elsewhere = tempfile::TempDir::new().expect("tempdir");
        env.remove(MISE_SHELL);
        env.remove(MISE_ORIG_PATH);
        env.set(DIRENV_DIR, format!("-{}", elsewhere.path().to_str().unwrap()));

        assert!(
            detect(project.path()).observed.is_empty(),
            "direnv managing another directory is not a yield for this one"
        );
    }

    /// S-018, C-049 — `MISE_SHELL` present yields, symmetric to direnv.
    #[test]
    fn detect_yields_mise_when_mise_shell_present_s018() {
        let env = test::env::lock();
        let project = tempfile::TempDir::new().expect("tempdir");
        env.remove(DIRENV_DIR);
        env.set(MISE_SHELL, "zsh");
        env.remove(MISE_ORIG_PATH);

        let verdict = detect(project.path());

        assert_eq!(
            verdict.observed,
            vec![Observation {
                tool: Tool::Mise,
                signal: format!("{MISE_SHELL}=zsh")
            }]
        );
    }

    /// S-018, C-049 — `__MISE_ORIG_PATH` alone (MISE_SHELL absent) also
    /// yields: the two mise sentinels are an OR, not a requirement that both
    /// be set.
    #[test]
    fn detect_yields_mise_when_only_orig_path_present_s018() {
        let env = test::env::lock();
        let project = tempfile::TempDir::new().expect("tempdir");
        env.remove(DIRENV_DIR);
        env.remove(MISE_SHELL);
        env.set(MISE_ORIG_PATH, "/usr/bin:/bin");

        let verdict = detect(project.path());

        assert_eq!(
            verdict.observed,
            vec![Observation {
                tool: Tool::Mise,
                signal: format!("{MISE_ORIG_PATH}=/usr/bin:/bin")
            }]
        );
    }

    /// S-019, C-020 — an `.envrc` on disk with direnv not live in this shell
    /// must not yield. Proves detection keys on session state, not on a file
    /// lying on disk: this test writes a real `.envrc` into the project and
    /// still expects an empty verdict because none of the three env vars are
    /// set.
    #[test]
    fn detect_no_yield_when_envrc_on_disk_but_direnv_not_active_s019() {
        let env = test::env::lock();
        env.remove(DIRENV_DIR);
        env.remove(MISE_SHELL);
        env.remove(MISE_ORIG_PATH);

        let project = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(project.path().join(".envrc"), "export FOO=bar\n").expect("write .envrc");

        let verdict = detect(project.path());

        assert!(
            verdict.observed.is_empty(),
            "an .envrc on disk is not a live direnv session (C-020)"
        );
    }

    /// S-020, C-049 — `DIRENV_DIR` naming a *different* (ancestor) directory
    /// must not yield for the nested project.
    #[test]
    fn detect_no_yield_when_direnv_dir_names_different_directory_s020() {
        let env = test::env::lock();
        let ancestor = tempfile::TempDir::new().expect("tempdir");
        let project_dir = ancestor.path().join("nested");
        std::fs::create_dir(&project_dir).expect("mkdir nested project");

        env.set(DIRENV_DIR, ancestor.path().to_str().expect("utf8 tempdir path"));
        env.remove(MISE_SHELL);
        env.remove(MISE_ORIG_PATH);

        let verdict = detect(&project_dir);

        assert!(
            verdict.observed.is_empty(),
            "DIRENV_DIR naming an ancestor is direnv managing a different project (S-020)"
        );
    }

    /// A-37 — both sentinels set and matching fire independently: two
    /// observations, direnv before mise (detection order). Red state under
    /// an `elif`-coupled implementation: see the fault-injection note in the
    /// WP-4 completion report — this is also the test that mutation targets.
    #[test]
    fn detect_both_sentinels_fire_independently_a37() {
        let env = test::env::lock();
        let project = tempfile::TempDir::new().expect("tempdir");
        let project_dir = project.path();

        env.set(DIRENV_DIR, project_dir.to_str().expect("utf8 tempdir path"));
        env.set(MISE_SHELL, "fish");
        env.remove(MISE_ORIG_PATH);

        let verdict = detect(project_dir);

        assert_eq!(
            verdict.observed.len(),
            2,
            "both live sentinels must yield two independent observations (A-37)"
        );
        assert_eq!(verdict.observed[0].tool, Tool::Direnv);
        assert_eq!(verdict.observed[1].tool, Tool::Mise);
    }
}
