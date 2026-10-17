//! Git observation for a session's cwd. Absence of git, or a cwd outside a
//! repository, yields `None`; the session is still usable.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;

use crate::model::GitInfo;
use crate::providers::process::{find_in_path, run};

const GIT_TIMEOUT: Duration = Duration::from_secs(3);

pub fn git_binary() -> Option<PathBuf> {
    find_in_path("git")
}

/// Repository root and current branch of `cwd`. Detached HEAD gives a root
/// with no branch. Uses `symbolic-ref` so an unborn branch is still named.
pub async fn observe(cwd: &Path) -> Option<GitInfo> {
    let git = git_binary()?;
    let root = git_line(&git, cwd, &["rev-parse", "--show-toplevel"]).await?;
    let branch = git_line(&git, cwd, &["symbolic-ref", "--short", "-q", "HEAD"]).await;
    Some(GitInfo {
        root: Some(PathBuf::from(root)),
        branch,
        observed_at: Utc::now(),
    })
}

async fn git_line(git: &Path, cwd: &Path, args: &[&str]) -> Option<String> {
    let out = run(git, args, Some(cwd), GIT_TIMEOUT).await.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn observes_root_and_branch_of_fresh_repo() {
        let Some(git) = git_binary() else {
            eprintln!("git not in PATH; skipping");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        run(
            &git,
            &["-c", "init.defaultBranch=main", "init", "-q"],
            Some(dir.path()),
            GIT_TIMEOUT,
        )
        .await
        .unwrap();
        let info = observe(dir.path()).await.unwrap();
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(
            info.root.unwrap().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn non_repo_is_none() {
        if git_binary().is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("plain dir with spaces");
        std::fs::create_dir(&outside).unwrap();
        // A temp dir under a repo would still resolve; guard against that.
        if observe(dir.path()).await.is_some() {
            eprintln!("temp dir is inside a git repo; skipping");
            return;
        }
        assert!(observe(&outside).await.is_none());
    }
}
