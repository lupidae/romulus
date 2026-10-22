//! Where a run comes from, read from the CI's environment where it is
//! cheap: the pull request, the branch, the commit and the run's page. It
//! names the draft the migrations go into (`PR #42: add-refunds`), so every
//! push of one pull request lands in the same draft, and heads the report.
//! GitHub Actions and GitLab CI are read; anywhere else, nothing (give
//! `--label`).

use serde_json::Value;

/// A run's context; every part optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    /// `GitHub Actions`, `GitLab CI`.
    pub ci: Option<&'static str>,
    /// The pull (or merge) request's number.
    pub pr: Option<u64>,
    /// The branch the change is on (a pull request's head).
    pub branch: Option<String>,
    /// The commit tested (a pull request's head, not GitHub's merge commit).
    pub commit: Option<String>,
    /// The CI run's page.
    pub run_url: Option<String>,
}

/// A draft's name, at most (`drafts.name`).
const NAME_CHARS: usize = 200;

impl Context {
    /// The context of the environment `var` reads, and the GitHub event's
    /// payload `event` (the file `GITHUB_EVENT_PATH` names, read).
    pub fn detect(var: &dyn Fn(&str) -> Option<String>, event: Option<&Value>) -> Self {
        let var = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        if var("GITHUB_ACTIONS").as_deref() == Some("true") {
            let pull = event.and_then(|e| e.get("pull_request"));
            let pr = pull
                .and_then(|p| p["number"].as_u64())
                .or_else(|| github_pr(var("GITHUB_REF").as_deref()));
            let run_url = match (
                var("GITHUB_SERVER_URL"),
                var("GITHUB_REPOSITORY"),
                var("GITHUB_RUN_ID"),
            ) {
                (Some(server), Some(repo), Some(run)) => {
                    Some(format!("{server}/{repo}/actions/runs/{run}"))
                }
                _ => None,
            };
            return Self {
                ci: Some("GitHub Actions"),
                pr,
                branch: var("GITHUB_HEAD_REF").or_else(|| var("GITHUB_REF_NAME")),
                commit: pull
                    .and_then(|p| p["head"]["sha"].as_str())
                    .map(str::to_owned)
                    .or_else(|| var("GITHUB_SHA")),
                run_url,
            };
        }
        if var("GITLAB_CI").as_deref() == Some("true") {
            return Self {
                ci: Some("GitLab CI"),
                pr: var("CI_MERGE_REQUEST_IID").and_then(|n| n.parse().ok()),
                branch: var("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME")
                    .or_else(|| var("CI_COMMIT_REF_NAME")),
                commit: var("CI_COMMIT_SHA"),
                run_url: var("CI_PIPELINE_URL").or_else(|| var("CI_JOB_URL")),
            };
        }
        Self::default()
    }

    /// The draft's name: `PR #42: add-refunds`, `main`, or `None` (romulus
    /// names it, and nothing is reused).
    pub fn label(&self) -> Option<String> {
        let label = match (self.pr, &self.branch) {
            (Some(pr), Some(branch)) => format!("PR #{pr}: {branch}"),
            (Some(pr), None) => format!("PR #{pr}"),
            (None, Some(branch)) => format!("Branch {branch}"),
            (None, None) => return None,
        };
        Some(clip(&label))
    }

    /// The commit, short.
    pub fn short_commit(&self) -> Option<&str> {
        self.commit.as_deref().map(|c| c.get(..7).unwrap_or(c))
    }
}

/// A label as a draft's name: one line, at most [`NAME_CHARS`].
pub fn clip(label: &str) -> String {
    let line: String = label
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if line.chars().count() <= NAME_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(NAME_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// `refs/pull/42/merge` → 42.
fn github_pr(reference: Option<&str>) -> Option<u64> {
    reference?
        .strip_prefix("refs/pull/")?
        .split('/')
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn a_github_pull_request_names_its_number_branch_and_head_commit() {
        let event =
            json!({ "pull_request": { "number": 42, "head": { "sha": "0123456789abcdef" } } });
        let context = Context::detect(
            &env(&[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_EVENT_NAME", "pull_request"),
                ("GITHUB_REF", "refs/pull/42/merge"),
                ("GITHUB_HEAD_REF", "add-refunds"),
                ("GITHUB_REF_NAME", "42/merge"),
                ("GITHUB_SHA", "ffffffffffffffff"),
                ("GITHUB_SERVER_URL", "https://github.com"),
                ("GITHUB_REPOSITORY", "acme/shop"),
                ("GITHUB_RUN_ID", "991"),
            ]),
            Some(&event),
        );
        assert_eq!(context.pr, Some(42));
        assert_eq!(context.branch.as_deref(), Some("add-refunds"));
        // The head, not the merge commit GitHub tests.
        assert_eq!(context.short_commit(), Some("0123456"));
        assert_eq!(
            context.run_url.as_deref(),
            Some("https://github.com/acme/shop/actions/runs/991")
        );
        assert_eq!(context.label().as_deref(), Some("PR #42: add-refunds"));
    }

    #[test]
    fn a_github_push_names_its_branch() {
        let context = Context::detect(
            &env(&[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REF", "refs/heads/main"),
                ("GITHUB_REF_NAME", "main"),
                ("GITHUB_HEAD_REF", ""),
                ("GITHUB_SHA", "abcdef0123"),
            ]),
            None,
        );
        assert_eq!(context.pr, None);
        assert_eq!(context.label().as_deref(), Some("Branch main"));
        assert_eq!(context.short_commit(), Some("abcdef0"));
    }

    #[test]
    fn the_pull_request_number_falls_back_on_the_ref() {
        let context = Context::detect(
            &env(&[
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REF", "refs/pull/7/merge"),
            ]),
            None,
        );
        assert_eq!(context.label().as_deref(), Some("PR #7"));
    }

    #[test]
    fn a_gitlab_merge_request() {
        let context = Context::detect(
            &env(&[
                ("GITLAB_CI", "true"),
                ("CI_MERGE_REQUEST_IID", "12"),
                ("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", "orders"),
                ("CI_COMMIT_REF_NAME", "orders"),
                ("CI_COMMIT_SHA", "1234567890"),
                ("CI_PIPELINE_URL", "https://gitlab.example/p/-/pipelines/5"),
            ]),
            None,
        );
        assert_eq!(context.ci, Some("GitLab CI"));
        assert_eq!(context.label().as_deref(), Some("PR #12: orders"));
    }

    #[test]
    fn elsewhere_nothing() {
        let context = Context::detect(&env(&[("CI", "true")]), None);
        assert_eq!(context, Context::default());
        assert_eq!(context.label(), None);
    }

    #[test]
    fn labels_fit_a_draft_name() {
        let long = "x".repeat(300);
        let clipped = clip(&format!("PR #1: {long}"));
        assert_eq!(clipped.chars().count(), 200);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip(" a\nb "), "a b");
    }
}
