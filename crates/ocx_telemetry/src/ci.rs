// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! CI provider detection from the environment.

use crate::gate::truthy;

/// The CI variables the resource reads, taken from the environment by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CiEnvironment {
    /// `GITHUB_ACTIONS` is truthy.
    pub github_actions: bool,
    /// `GITLAB_CI` is truthy.
    pub gitlab_ci: bool,
    /// `CI` is truthy.
    pub ci: bool,
    /// `GITHUB_RUN_ID`.
    pub github_run_id: Option<String>,
    /// `GITHUB_JOB`.
    pub github_job: Option<String>,
    /// `CI_PIPELINE_ID`.
    pub gitlab_pipeline_id: Option<String>,
    /// `CI_JOB_NAME`.
    pub gitlab_job_name: Option<String>,
    /// `CI_JOB_ID`.
    pub gitlab_job_id: Option<String>,
}

impl CiEnvironment {
    /// Reads the CI variables through `ocx_env`; an empty variable counts as unset.
    pub fn from_env() -> Self {
        Self {
            github_actions: truthy(&ocx_env::GITHUB_ACTIONS),
            gitlab_ci: truthy(&ocx_env::GITLAB_CI),
            ci: truthy(&ocx_env::CI),
            github_run_id: ocx_env::GITHUB_RUN_ID.get(),
            github_job: ocx_env::GITHUB_JOB.get(),
            gitlab_pipeline_id: ocx_env::CI_PIPELINE_ID.get(),
            gitlab_job_name: ocx_env::CI_JOB_NAME.get(),
            gitlab_job_id: ocx_env::CI_JOB_ID.get(),
        }
    }

    /// The `sh.ocx.ci.provider` value: `github`, else `gitlab`, else `other`; `None` outside CI.
    pub fn provider(&self) -> Option<&'static str> {
        if self.github_actions {
            Some("github")
        } else if self.gitlab_ci {
            Some("gitlab")
        } else if self.ci {
            Some("other")
        } else {
            None
        }
    }

    /// The resource attributes this environment contributes, in key order. Empty values are
    /// omitted, and only the detected provider's variables are consulted.
    pub(crate) fn attributes(&self) -> Vec<(&'static str, String)> {
        let Some(provider) = self.provider() else {
            return Vec::new();
        };
        let (run_id, task_name, task_run_id) = match provider {
            "github" => (&self.github_run_id, &self.github_job, &None),
            "gitlab" => (&self.gitlab_pipeline_id, &self.gitlab_job_name, &self.gitlab_job_id),
            _ => (&None, &None, &None),
        };
        let mut attributes = vec![("sh.ocx.ci.provider", provider.to_owned())];
        for (key, value) in [
            ("cicd.pipeline.run.id", run_id),
            ("cicd.pipeline.task.name", task_name),
            ("cicd.pipeline.task.run.id", task_run_id),
        ] {
            if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
                attributes.push((key, value.clone()));
            }
        }
        attributes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(value: &str) -> Option<String> {
        Some(value.to_owned())
    }

    fn keys(ci: &CiEnvironment) -> Vec<(&'static str, String)> {
        ci.attributes()
    }

    fn pair(key: &'static str, value: &str) -> (&'static str, String) {
        (key, value.to_owned())
    }

    #[test]
    fn ci_detection_table() {
        let github = CiEnvironment {
            github_actions: true,
            ci: true,
            github_run_id: some("11"),
            github_job: some("build"),
            // GitLab ids never leak into a GitHub run.
            gitlab_job_id: some("9"),
            ..CiEnvironment::default()
        };
        assert_eq!(
            keys(&github),
            [
                pair("sh.ocx.ci.provider", "github"),
                pair("cicd.pipeline.run.id", "11"),
                pair("cicd.pipeline.task.name", "build"),
            ]
        );

        let gitlab = CiEnvironment {
            gitlab_ci: true,
            ci: true,
            gitlab_pipeline_id: some("22"),
            gitlab_job_name: some("test"),
            gitlab_job_id: some("33"),
            github_run_id: some("ignored"),
            ..CiEnvironment::default()
        };
        assert_eq!(
            keys(&gitlab),
            [
                pair("sh.ocx.ci.provider", "gitlab"),
                pair("cicd.pipeline.run.id", "22"),
                pair("cicd.pipeline.task.name", "test"),
                pair("cicd.pipeline.task.run.id", "33"),
            ]
        );

        let other = CiEnvironment {
            ci: true,
            github_run_id: some("ignored"),
            ..CiEnvironment::default()
        };
        assert_eq!(keys(&other), [pair("sh.ocx.ci.provider", "other")]);

        assert!(keys(&CiEnvironment::default()).is_empty());
        assert_eq!(CiEnvironment::default().provider(), None);

        let empty_ids = CiEnvironment {
            gitlab_ci: true,
            gitlab_pipeline_id: some(""),
            gitlab_job_name: some(""),
            gitlab_job_id: None,
            ..CiEnvironment::default()
        };
        assert_eq!(keys(&empty_ids), [pair("sh.ocx.ci.provider", "gitlab")]);
    }

    #[test]
    fn from_env_reads_the_ci_variables() {
        let env = ocx_env::overrides::lock();
        for var in [
            &ocx_env::GITHUB_ACTIONS,
            &ocx_env::CI,
            &ocx_env::GITHUB_RUN_ID,
            &ocx_env::GITHUB_JOB,
            &ocx_env::CI_JOB_NAME,
        ] {
            env.remove(var);
        }
        env.set(&ocx_env::GITLAB_CI, "true");
        env.set(&ocx_env::CI_PIPELINE_ID, "77");
        env.set(&ocx_env::CI_JOB_ID, "");
        let ci = CiEnvironment::from_env();
        assert!(ci.gitlab_ci && !ci.github_actions && !ci.ci);
        assert_eq!(ci.gitlab_pipeline_id.as_deref(), Some("77"));
        assert_eq!(ci.gitlab_job_id, None);
    }
}
