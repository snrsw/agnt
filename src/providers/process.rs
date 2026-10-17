//! Child-process helpers shared by the adapters. Arguments are always passed
//! as arrays; user input never becomes part of a shell string.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use tokio::process::Command;

use super::AdapterError;

pub const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Locate an executable on `PATH` without invoking a shell.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Run `program args...` in `cwd` with a timeout. Non-zero exit is an error
/// carrying stderr; the child is killed on timeout.
pub async fn run(
    program: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<Output, AdapterError> {
    let command_label = format!(
        "{} {}",
        program.file_name().unwrap_or_default().to_string_lossy(),
        args.join(" ")
    )
    .trim_end()
    .to_string();

    let mut cmd = Command::new(program);
    cmd.args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(result) => result?,
        Err(_) => {
            return Err(AdapterError::Timeout {
                command: command_label,
                timeout_ms: timeout.as_millis() as u64,
            });
        }
    };
    if !output.status.success() {
        return Err(AdapterError::CliFailed {
            command: command_label,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output)
}

/// First line of `<program> --version`.
pub async fn version_of(program: Option<&Path>, name: &str) -> Result<String, AdapterError> {
    let program = program.ok_or_else(|| AdapterError::CliMissing {
        binary: name.to_string(),
    })?;
    let out = run(program, &["--version"], None, VERSION_TIMEOUT).await?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().next().unwrap_or_default().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sh_and_not_nonsense() {
        assert!(find_in_path("sh").is_some());
        assert!(find_in_path("agnt-definitely-missing-binary").is_none());
    }

    #[tokio::test]
    async fn failing_command_reports_stderr() {
        let sh = find_in_path("sh").unwrap();
        let err = run(&sh, &["-c", "echo bad >&2; exit 3"], None, VERSION_TIMEOUT)
            .await
            .unwrap_err();
        match err {
            AdapterError::CliFailed { stderr, .. } => assert_eq!(stderr, "bad"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_kills_child() {
        let sh = find_in_path("sh").unwrap();
        let err = run(&sh, &["-c", "sleep 5"], None, Duration::from_millis(100))
            .await
            .unwrap_err();
        assert!(matches!(err, AdapterError::Timeout { .. }));
    }
}
