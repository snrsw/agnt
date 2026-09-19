//! Dropping into the user's shell, and finding out where they left it.
//!
//! The shell gets the terminal itself, the same way an attached session does.
//! That is the whole trick: nothing is emulated, captured or re-implemented,
//! so tab completion, abbreviations, colours, the prompt and history are the
//! real ones — because it *is* the real shell on a real terminal.
//!
//! The one thing agnt still wants is where the shell ended up, and a
//! process's working directory dies with it. So the shell is asked to write
//! it down on its way out, through the hook each family provides. A shell
//! with no such hook still runs; it only loses the directory, and says so.

use std::path::{Path, PathBuf};

/// Where the shell writes its final directory. Read by agnt once the shell
/// has exited.
pub const PWD_ENV: &str = "AGNT_PWD_FILE";

/// The families whose exit hook agnt knows how to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Fish,
    Bash,
    Zsh,
    /// Anything else. It runs; the directory is not read back.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub program: PathBuf,
    pub kind: ShellKind,
}

/// How to start the shell, and the scratch directory that has to outlive it.
pub struct Launch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub pwd_file: PathBuf,
    /// Holds the temporary start-up files. Dropping it deletes them, so it is
    /// kept until the shell has exited.
    pub _scratch: tempfile::TempDir,
}

impl Shell {
    /// `AGNT_SHELL`, else `$SHELL`, else `/bin/sh`.
    pub fn detect() -> Self {
        let program = std::env::var_os("AGNT_SHELL")
            .or_else(|| std::env::var_os("SHELL"))
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Self::at(program)
    }

    pub fn at(program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        let kind = match program.file_name().and_then(|n| n.to_str()) {
            Some("fish") => ShellKind::Fish,
            Some("bash") => ShellKind::Bash,
            Some("zsh") => ShellKind::Zsh,
            _ => ShellKind::Other,
        };
        Self { program, kind }
    }

    /// Whether the shell's directory will come back.
    pub fn tracks_directory(&self) -> bool {
        self.kind != ShellKind::Other
    }

    /// Build the command that starts an interactive shell which records where
    /// it ends up.
    ///
    /// Each family's hook is used as documented, and none of them replaces
    /// the user's own configuration: fish takes an extra init command, and
    /// the two POSIX shells are pointed at a start-up file that sources
    /// theirs first. Getting this wrong would drop someone into a shell
    /// without their prompt or their aliases, which is worse than not
    /// tracking the directory at all.
    pub fn launch(&self) -> std::io::Result<Launch> {
        let scratch = tempfile::tempdir()?;
        let pwd_file = scratch.path().join("pwd");
        let mut env = vec![(PWD_ENV.to_string(), pwd_file.to_string_lossy().into_owned())];
        let args: Vec<String> = match self.kind {
            ShellKind::Fish => vec![
                "--init-command".into(),
                format!("function __agnt_record --on-event fish_exit; pwd >\"${PWD_ENV}\"; end"),
                "--interactive".into(),
            ],
            ShellKind::Bash => {
                let rc = scratch.path().join("bashrc");
                std::fs::write(
                    &rc,
                    format!("[ -f ~/.bashrc ] && . ~/.bashrc\ntrap 'pwd >\"${PWD_ENV}\"' EXIT\n"),
                )?;
                vec!["--rcfile".into(), rc.to_string_lossy().into_owned(), "-i".into()]
            }
            ShellKind::Zsh => {
                // zsh reads its start-up files from ZDOTDIR, so the real one
                // is passed along for the copies to source.
                let home = std::env::var("HOME").unwrap_or_default();
                let real = std::env::var("ZDOTDIR").unwrap_or(home);
                for name in [".zshenv", ".zshrc"] {
                    std::fs::write(
                        scratch.path().join(name),
                        format!(
                            "[ -f \"$AGNT_REAL_ZDOTDIR/{name}\" ] && . \"$AGNT_REAL_ZDOTDIR/{name}\"\ntrap 'pwd >\"${PWD_ENV}\"' EXIT\n"
                        ),
                    )?;
                }
                env.push(("AGNT_REAL_ZDOTDIR".into(), real));
                env.push(("ZDOTDIR".into(), scratch.path().to_string_lossy().into_owned()));
                vec!["-i".into()]
            }
            ShellKind::Other => vec!["-i".into()],
        };
        Ok(Launch {
            args,
            env,
            pwd_file,
            _scratch: scratch,
        })
    }
}

/// The directory the shell left behind, if it wrote one and it still exists.
pub fn read_back_cwd(pwd_file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(pwd_file).ok()?;
    let path = PathBuf::from(text.trim());
    path.is_dir().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_and_read(shell: &Shell, start: &Path, command: &str) -> Option<PathBuf> {
        let launch = shell.launch().unwrap();
        let mut cmd = std::process::Command::new(&shell.program);
        // `-c` after the interactive flags exercises the same start-up files
        // and exit hook a real session uses, without needing a terminal.
        cmd.args(&launch.args)
            .arg("-c")
            .arg(command)
            .current_dir(start)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (k, v) in &launch.env {
            cmd.env(k, v);
        }
        let _ = cmd.status();
        read_back_cwd(&launch.pwd_file)
    }

    #[test]
    fn shells_are_told_apart_by_name() {
        assert_eq!(Shell::at("/usr/bin/fish").kind, ShellKind::Fish);
        assert_eq!(Shell::at("/bin/bash").kind, ShellKind::Bash);
        assert_eq!(Shell::at("/bin/zsh").kind, ShellKind::Zsh);
        assert_eq!(Shell::at("/opt/nu").kind, ShellKind::Other);
        assert!(Shell::at("/bin/bash").tracks_directory());
        assert!(!Shell::at("/opt/nu").tracks_directory());
    }

    /// The point of the whole exercise: the directory the user walked to
    /// comes back. Run against whichever real shells this machine has.
    #[test]
    fn the_directory_comes_back_from_a_real_shell() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();

        let mut tried = 0;
        for name in ["fish", "bash", "zsh"] {
            let Some(path) = crate::providers::process::find_in_path(name) else {
                continue;
            };
            tried += 1;
            let shell = Shell::at(path);
            let got = run_and_read(&shell, dir.path(), "cd sub");
            assert_eq!(
                got.map(|p| p.canonicalize().unwrap()),
                Some(sub.canonicalize().unwrap()),
                "{name} did not report its directory"
            );
        }
        assert!(tried > 0, "no shell to test against");
    }

    /// A shell with no hook agnt knows still starts. It just has nothing to
    /// read back, which is a missing directory rather than a broken shell.
    #[test]
    fn an_unknown_shell_still_launches_without_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Shell::at("/bin/sh");
        assert_eq!(shell.kind, ShellKind::Other);
        let launch = shell.launch().unwrap();
        assert_eq!(launch.args, vec!["-i"]);
        assert_eq!(read_back_cwd(&launch.pwd_file), None);
        assert_eq!(run_and_read(&shell, dir.path(), "cd /"), None);
    }

    #[test]
    fn a_missing_or_stale_directory_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pwd");
        assert_eq!(read_back_cwd(&file), None);
        std::fs::write(&file, "/definitely/not/here\n").unwrap();
        assert_eq!(read_back_cwd(&file), None);
        std::fs::write(&file, format!("{}\n", dir.path().display())).unwrap();
        assert_eq!(read_back_cwd(&file), Some(dir.path().to_path_buf()));
    }
}
