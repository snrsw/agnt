//! The one-line shell behind the prompt's shell mode.
//!
//! Commands run through the user's own shell so their functions, `PATH` and
//! environment are the ones they expect. Two things that shell would do for
//! them at a terminal have to be done here instead, because there is no
//! terminal: abbreviations are expanded, and the directory the shell ends in
//! is read back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Printed by the shell after each command so the directory it ended in can
/// be read back. Deliberately dull text: it has to survive every shell's
/// `printf` and never be mistaken for output.
pub const PWD_MARKER: &str = "__agnt_pwd__:";

/// How long a command may run before it is killed. Long enough for a build,
/// short enough that something waiting on a terminal it does not have gives
/// up rather than wedging the list.
pub const TIMEOUT: Duration = Duration::from_secs(60);

/// Lines the output pane keeps. Older lines fall off the top.
pub const SCROLLBACK: usize = 500;

/// How long to wait for the shell to list its abbreviations at startup.
const ABBR_TIMEOUT: Duration = Duration::from_secs(10);

/// The two shell families that matter here. They differ in how the exit
/// status is spelled, how history is turned off, and where their
/// abbreviations live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Fish,
    Posix,
}

/// An abbreviation or alias: text the shell's line editor would have expanded
/// before running, which nothing expands when the command is handed to `-c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abbreviation {
    pub expansion: String,
    /// Fish abbreviations default to expanding only in command position.
    /// `--position anywhere` makes them expand as any word.
    pub anywhere: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub program: PathBuf,
    pub kind: ShellKind,
}

impl Shell {
    /// `AGNT_SHELL`, else `$SHELL`, else `/bin/sh`. The override exists so a
    /// shell that is pleasant to live in but awkward to script can be swapped
    /// for one that is not, without changing the login shell.
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
            _ => ShellKind::Posix,
        };
        Self { program, kind }
    }

    /// The line as handed to `-c`: the command, then a trailer that prints
    /// the directory the shell is now in and exits with the command's own
    /// status.
    ///
    /// The status has to be saved before the `printf`, or the shell reports
    /// the trailer's success and `cd nope` reads as fine. Saving it is the
    /// one place the two families differ.
    pub fn script(&self, line: &str) -> String {
        let save = match self.kind {
            ShellKind::Fish => "set __agnt_s $status",
            ShellKind::Posix => "__agnt_s=$?",
        };
        format!("{line}\n{save}\nprintf '\\n{PWD_MARKER}%s\\n' \"$PWD\"\nexit $__agnt_s")
    }

    /// Environment that keeps these commands out of the user's history. The
    /// prompt is for getting somewhere, not a session anyone wants to scroll
    /// back through later, and silently filling a history file with it would
    /// be rude.
    pub fn history_env(&self) -> Vec<(&'static str, &'static str)> {
        match self.kind {
            ShellKind::Fish => vec![("fish_private_mode", "1")],
            ShellKind::Posix => vec![("HISTFILE", "/dev/null"), ("HISTSIZE", "0")],
        }
    }

    fn command(&self, cwd: &Path, script: &str, interactive: bool) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.program);
        if interactive {
            // Abbreviations and aliases are usually defined behind an
            // "is this interactive?" guard, so listing them needs a shell
            // that answers yes.
            cmd.arg("-i");
        }
        cmd.arg("-c")
            .arg(script)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        for (k, v) in self.history_env() {
            cmd.env(k, v);
        }
        cmd
    }

    /// Run one line: captured stdout and stderr, and whether it succeeded.
    /// Stdin is closed so nothing can wait on input.
    pub async fn run(&self, cwd: &Path, line: &str) -> (String, String, bool) {
        let mut cmd = self.command(cwd, &self.script(line), false);
        match tokio::time::timeout(TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) => (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                out.status.success(),
            ),
            Ok(Err(e)) => (
                String::new(),
                format!("could not run {}: {e}", self.program.display()),
                false,
            ),
            Err(_) => (
                String::new(),
                format!(
                    "killed after {}s: shell-mode commands run without a terminal",
                    TIMEOUT.as_secs()
                ),
                false,
            ),
        }
    }

    /// Ask the shell for its abbreviations, or aliases, whichever it has.
    ///
    /// Both are features of the interactive line editor: nothing expands them
    /// when a command is handed to `-c`, and they are usually only *defined*
    /// for an interactive shell in the first place. So the listing runs
    /// interactively, and agnt expands them itself at run time.
    pub async fn load_abbreviations(&self, cwd: &Path) -> BTreeMap<String, Abbreviation> {
        let script = match self.kind {
            ShellKind::Fish => "abbr --show",
            ShellKind::Posix => "alias",
        };
        let mut cmd = self.command(cwd, script, true);
        let Ok(Ok(out)) = tokio::time::timeout(ABBR_TIMEOUT, cmd.output()).await else {
            return BTreeMap::new();
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let parse = match self.kind {
            ShellKind::Fish => parse_fish_abbr,
            ShellKind::Posix => parse_posix_alias,
        };
        text.lines().filter_map(parse).collect()
    }
}

/// Split the marker trailer off captured stdout: the output as the user
/// should see it, and the directory the shell ended in, if the trailer ran.
/// A command that exits the shell leaves no trailer, which reads as
/// "directory unchanged".
pub fn split_pwd_marker(stdout: &str) -> (String, Option<PathBuf>) {
    let Some(at) = stdout.rfind(PWD_MARKER) else {
        return (stdout.to_string(), None);
    };
    let pwd = stdout[at + PWD_MARKER.len()..]
        .lines()
        .next()
        .map(|s| PathBuf::from(s.trim()));
    // The trailer starts with its own newline; that one is ours, not the
    // command's, so it goes.
    let head = stdout[..at].strip_suffix('\n').unwrap_or(&stdout[..at]);
    (head.to_string(), pwd.filter(|p| !p.as_os_str().is_empty()))
}

/// One line of `abbr --show`, which prints definitions in a form that could
/// be fed back to fish: `abbr -a --position command -- ls 'eza --icons'`.
///
/// Regex and function abbreviations are skipped: neither can be expanded by
/// looking a name up, and half-expanding one would be worse than leaving it
/// to fail as an unknown command.
pub fn parse_fish_abbr(line: &str) -> Option<(String, Abbreviation)> {
    let line = line.trim();
    let (opts, rest) = line.split_once(" -- ")?;
    if !opts.starts_with("abbr") || opts.contains("--regex") || opts.contains("--function") {
        return None;
    }
    let anywhere = opts.contains("anywhere");
    let (name, expansion) = rest.split_once(char::is_whitespace)?;
    let expansion = unquote_fish(expansion.trim());
    (!name.is_empty() && !expansion.is_empty())
        .then(|| (name.to_string(), Abbreviation { expansion, anywhere }))
}

/// One line of `alias`: `alias ll='ls -l'`, or bash's `alias ll='ls -l'`.
pub fn parse_posix_alias(line: &str) -> Option<(String, Abbreviation)> {
    let line = line.trim().strip_prefix("alias ").unwrap_or(line.trim());
    let (name, value) = line.split_once('=')?;
    let expansion = unquote_posix(value.trim());
    (!name.is_empty() && !expansion.is_empty() && !name.contains(char::is_whitespace)).then(|| {
        (
            name.to_string(),
            Abbreviation {
                expansion,
                anywhere: false,
            },
        )
    })
}

fn unquote_fish(s: &str) -> String {
    match s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        Some(inner) => inner.replace("\\'", "'").replace("\\\\", "\\"),
        None => s.to_string(),
    }
}

fn unquote_posix(s: &str) -> String {
    match s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        // POSIX shells cannot escape a quote inside single quotes; they close
        // and reopen around it.
        Some(inner) => inner.replace("'\\''", "'"),
        None => s.trim_matches('"').to_string(),
    }
}

/// Expand a typed line's abbreviations, the way the shell's line editor would
/// have done before running it.
///
/// Only whole words are replaced, and by default only in command position —
/// the start of the line, or after `;`, `|`, `&&`, `||`, or one of fish's
/// `and` / `or` / `not` / `begin`. Text inside quotes is left alone, so
/// `echo "ls"` keeps its word.
pub fn expand(line: &str, abbrs: &BTreeMap<String, Abbreviation>) -> String {
    if abbrs.is_empty() {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut word = String::new();
    let mut command_position = true;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    // Flush the word just read, expanding it when it is a word the shell
    // would have expanded here.
    fn flush(out: &mut String, word: &mut String, at_command: bool, abbrs: &BTreeMap<String, Abbreviation>) {
        if word.is_empty() {
            return;
        }
        match abbrs.get(word.as_str()) {
            Some(a) if at_command || a.anywhere => out.push_str(&a.expansion),
            _ => out.push_str(word),
        }
        word.clear();
    }

    for c in line.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            word.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\\' => {
                word.push(c);
                escaped = true;
            }
            '\'' | '"' => {
                word.push(c);
                quote = Some(c);
            }
            c if c.is_whitespace() => {
                // Only an actual word moves us out of command position. The
                // space after `&&` has no word before it, and reading it as
                // one would leave the next word looking like an argument.
                if !word.is_empty() {
                    let was = command_position;
                    flush(&mut out, &mut word, command_position, abbrs);
                    if was {
                        command_position = ends_in_keyword(&out);
                    }
                }
                out.push(c);
            }
            ';' | '|' | '&' | '(' => {
                flush(&mut out, &mut word, command_position, abbrs);
                out.push(c);
                command_position = true;
            }
            _ => word.push(c),
        }
    }
    flush(&mut out, &mut word, command_position, abbrs);
    out
}

/// Whether what has been written so far ends with a word that puts the next
/// one back in command position.
fn ends_in_keyword(out: &str) -> bool {
    matches!(
        out.split_whitespace().next_back(),
        Some("and" | "or" | "not" | "begin" | "!" | "command" | "builtin" | "sudo" | "time" | "env")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abbrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Abbreviation> {
        pairs
            .iter()
            .map(|(n, e)| {
                (
                    n.to_string(),
                    Abbreviation {
                        expansion: e.to_string(),
                        anywhere: false,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn fish_and_posix_are_told_apart_by_name() {
        assert_eq!(Shell::at("/usr/bin/fish").kind, ShellKind::Fish);
        assert_eq!(Shell::at("/bin/zsh").kind, ShellKind::Posix);
        assert_eq!(Shell::at("/bin/sh").kind, ShellKind::Posix);
    }

    #[test]
    fn history_is_off_in_both_families() {
        let fish = Shell::at("/usr/bin/fish").history_env();
        assert_eq!(fish, vec![("fish_private_mode", "1")]);
        let posix = Shell::at("/bin/bash").history_env();
        assert!(posix.contains(&("HISTFILE", "/dev/null")));
    }

    /// The shapes `abbr --show` prints, including the one the user's own
    /// home-manager config produces.
    #[test]
    fn fish_abbreviations_parse() {
        let (name, a) = parse_fish_abbr("abbr -a --position command -- ls 'eza --icons --git'").unwrap();
        assert_eq!(name, "ls");
        assert_eq!(a.expansion, "eza --icons --git");
        assert!(!a.anywhere);

        let (name, a) = parse_fish_abbr("abbr -a -- cat bat").unwrap();
        assert_eq!((name.as_str(), a.expansion.as_str()), ("cat", "bat"));

        let (_, a) = parse_fish_abbr("abbr -a --position anywhere -- L '| less'").unwrap();
        assert!(a.anywhere);

        // An expansion carrying its own quotes survives.
        let (_, a) = parse_fish_abbr(r#"abbr -a -- beep 'afplay "/System/x.aiff"'"#).unwrap();
        assert_eq!(a.expansion, r#"afplay "/System/x.aiff""#);

        // Not expandable by name: left alone rather than half-expanded.
        assert!(parse_fish_abbr("abbr -a --regex '\\d+' -- n 'x'").is_none());
        assert!(parse_fish_abbr("abbr -a --function f -- n").is_none());
        assert!(parse_fish_abbr("not an abbr line").is_none());
    }

    #[test]
    fn posix_aliases_parse() {
        let (name, a) = parse_posix_alias("alias ll='ls -l'").unwrap();
        assert_eq!((name.as_str(), a.expansion.as_str()), ("ll", "ls -l"));
        let (name, a) = parse_posix_alias("gs='git status'").unwrap();
        assert_eq!((name.as_str(), a.expansion.as_str()), ("gs", "git status"));
        // A quote inside, the way a POSIX shell spells it.
        let (_, a) = parse_posix_alias(r#"alias say='echo '\''hi'\'''"#).unwrap();
        assert_eq!(a.expansion, "echo 'hi'");
    }

    #[test]
    fn expansion_replaces_whole_words_in_command_position() {
        let a = abbrs(&[("ls", "eza --icons"), ("cat", "bat")]);
        assert_eq!(expand("ls", &a), "eza --icons");
        assert_eq!(expand("ls -la", &a), "eza --icons -la");
        // Not in argument position.
        assert_eq!(expand("echo ls", &a), "echo ls");
        // But after a separator it is a command again.
        assert_eq!(expand("cd /tmp && ls", &a), "cd /tmp && eza --icons");
        assert_eq!(expand("true; ls", &a), "true; eza --icons");
        assert_eq!(expand("cat x | cat", &a), "bat x | bat");
        // Only whole words.
        assert_eq!(expand("lsof", &a), "lsof");
        assert_eq!(expand("./ls", &a), "./ls");
    }

    #[test]
    fn expansion_leaves_quoted_text_alone() {
        let a = abbrs(&[("ls", "eza --icons")]);
        assert_eq!(expand(r#"echo "ls""#, &a), r#"echo "ls""#);
        assert_eq!(expand("echo 'ls'", &a), "echo 'ls'");
        assert_eq!(expand(r#"grep "a;ls" f"#, &a), r#"grep "a;ls" f"#);
    }

    #[test]
    fn expansion_follows_words_that_open_a_command() {
        let a = abbrs(&[("ls", "eza --icons")]);
        assert_eq!(expand("sudo ls", &a), "sudo eza --icons");
        assert_eq!(expand("and ls", &a), "and eza --icons");
        // An anywhere abbreviation expands as an argument too.
        let mut any = abbrs(&[("L", "| less")]);
        any.get_mut("L").unwrap().anywhere = true;
        assert_eq!(expand("cat f L", &any), "cat f | less");
    }

    #[test]
    fn a_line_with_nothing_to_expand_comes_back_unchanged() {
        let a = abbrs(&[("ls", "eza")]);
        for line in ["git status", "", "  ", "echo 'a b'  |  wc -l"] {
            assert_eq!(expand(line, &a), line);
        }
        assert_eq!(expand("ls", &BTreeMap::new()), "ls");
    }

    #[test]
    fn the_pwd_trailer_is_split_off_cleanly() {
        // `echo a; echo b` ends with its own newline; the trailer adds one
        // more before the marker. Only the trailer's is stripped.
        let (out, pwd) = split_pwd_marker(&format!("a\nb\n\n{PWD_MARKER}/tmp/x\n"));
        assert_eq!(out, "a\nb\n");
        assert_eq!(pwd, Some(PathBuf::from("/tmp/x")));
        let (out, pwd) = split_pwd_marker("no trailer at all");
        assert_eq!(out, "no trailer at all");
        assert_eq!(pwd, None);
    }

    /// The status the pane reports is the command's, not the trailer's, in
    /// both shell families. Fish is this machine's shell, so its branch runs
    /// for real where fish is installed.
    #[tokio::test]
    async fn the_exit_status_is_the_commands_not_the_trailers() {
        let dir = tempfile::tempdir().unwrap();
        let mut shells = vec![Shell::at("/bin/sh")];
        if let Some(fish) = crate::providers::process::find_in_path("fish") {
            shells.push(Shell::at(fish));
        }
        for shell in shells {
            let (_, _, ok) = shell.run(dir.path(), "cd nope-not-here").await;
            assert!(!ok, "{:?}: a failed cd must not read as success", shell.program);
            let (stdout, _, ok) = shell.run(dir.path(), "echo fine").await;
            assert!(ok, "{:?}", shell.program);
            let (out, pwd) = split_pwd_marker(&stdout);
            assert_eq!(out.trim(), "fine", "{:?}", shell.program);
            assert_eq!(
                pwd.map(|p| p.canonicalize().unwrap()),
                Some(dir.path().canonicalize().unwrap()),
                "{:?}",
                shell.program
            );
        }
    }

    /// The listing has to run interactively, because that is the only shell
    /// for which abbreviations are usually defined at all.
    #[tokio::test]
    async fn fish_abbreviations_are_read_back_from_a_real_fish() {
        let Some(fish) = crate::providers::process::find_in_path("fish") else {
            eprintln!("fish not in PATH; skipping");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let conf = home.join(".config/fish");
        std::fs::create_dir_all(&conf).unwrap();
        // Defined behind the interactive guard, the way a generated config
        // does it — which is exactly what a non-interactive shell misses.
        std::fs::write(
            conf.join("config.fish"),
            "status is-interactive; and begin\n  abbr --add -- ls 'eza --icons --git'\n  abbr --add -- cat bat\nend\n",
        )
        .unwrap();

        // A fish that reads the throwaway config rather than the real one.
        let shell = Shell::at(fish);
        let mut cmd = tokio::process::Command::new(&shell.program);
        cmd.arg("-i")
            .arg("-c")
            .arg("abbr --show")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("fish_private_mode", "1")
            .stdin(std::process::Stdio::null());
        let out = cmd.output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let table: BTreeMap<String, Abbreviation> = text.lines().filter_map(parse_fish_abbr).collect();

        assert!(
            table.contains_key("ls"),
            "abbr --show gave: {text:?} (stderr {:?})",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(table["ls"].expansion, "eza --icons --git");
        assert_eq!(expand("ls -la", &table), "eza --icons --git -la");
    }
}
