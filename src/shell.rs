//! A real shell, embedded in the list.
//!
//! Shell mode runs `$SHELL` on a pseudo-terminal and draws its screen in
//! the text area at the bottom of the list — the same box the task prompt
//! uses, with the shell's own prompt on its bottom row. That means it is the
//! user's actual interactive shell: tab completion, abbreviations,
//! autosuggestions, colours, prompt and history all work, because fish (or
//! zsh, or bash) is doing them, not agnt.
//!
//! The box is a few rows tall, which is what a prompt needs: one command's
//! output and the line being typed. It is not a terminal to live in — the
//! list above it is what the screen is for.
//!
//! An earlier version handed single lines to `$SHELL -c` and reimplemented
//! the pieces an interactive shell would have provided — abbreviation
//! expansion, a `$PWD` trailer, a probe for whether the shell understood it.
//! All of that is gone: it was a reimplementation of a terminal, and a
//! terminal is a thing you can simply have. ADR-0001 deferred the PTY; this
//! takes it up, for the prompt only. Attaching to a provider's CLI still
//! hands over the whole terminal (`terminal::handoff`) rather than running it
//! in here.
//!
//! What agnt still needs from the shell is the one thing a terminal will not
//! tell it: which directory the shell is sitting in, so a new session can
//! start there. That is read from the operating system rather than asked of
//! the shell, which works whatever the shell is.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

/// How much of the shell's scrollback the emulator keeps.
pub const SCROLLBACK: usize = 1000;

/// Which shell to run: `AGNT_SHELL`, else the user's login shell, else
/// `$SHELL`, else `/bin/sh`.
///
/// `$SHELL` would be the conventional first choice, and it is the wrong one
/// here. A development shell rewrites it: inside `nix develop`, `$SHELL` is
/// the bash that nix provides, so agnt built and run that way would hand the
/// user bash however their own shell is set. The operating system's answer is
/// the one `chsh` wrote and no wrapper rewrites, so it is asked first.
///
/// No shell-specific syntax is generated any more, so this needs no knowledge
/// of what the shell is — it only has to be something that runs on a terminal.
pub fn program() -> PathBuf {
    if let Some(explicit) = std::env::var_os("AGNT_SHELL").filter(|s| !s.is_empty()) {
        return PathBuf::from(explicit);
    }
    login_shell()
        .or_else(|| {
            std::env::var_os("SHELL")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// The shell the operating system has on file for this user — what `chsh`
/// set, and what a terminal starts.
///
/// On macOS this is not in `/etc/passwd`; it lives in the directory service,
/// and `getpwuid` is what reads it on both platforms.
fn login_shell() -> Option<PathBuf> {
    // SAFETY: `getpwuid` returns a pointer into storage owned by libc, valid
    // until the next passwd call. It is read and copied immediately, and no
    // other passwd call happens in between.
    let shell = unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_shell).to_str().ok()?.to_owned()
    };
    (!shell.is_empty()).then(|| PathBuf::from(shell))
}

/// A running shell on a pseudo-terminal, and the screen it has drawn.
pub struct PtyShell {
    /// The parsed screen. The reader thread writes, the UI reads.
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pid: Option<u32>,
    size: (u16, u16),
    program: PathBuf,
}

impl std::fmt::Debug for PtyShell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyShell").field("pid", &self.pid).finish()
    }
}

impl PtyShell {
    /// Start `program` on a pty of the given size, in `cwd`.
    ///
    /// Which shell that is comes from [`program()`]; it is passed in rather
    /// than resolved here so a test can ask for a fast, predictable one
    /// without rewriting the environment other tests are reading.
    ///
    /// `on_output` is called whenever the shell writes something, so the
    /// event loop can redraw. The shell is started as a login shell so the
    /// user's configuration is the one they would get in a terminal.
    pub fn spawn(
        program: &Path,
        cwd: &Path,
        rows: u16,
        cols: u16,
        on_output: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = NativePtySystem::default()
            .openpty(size)
            .map_err(std::io::Error::other)?;

        let mut cmd = CommandBuilder::new(program);
        cmd.cwd(cwd);
        // A login shell, because that is what the user's terminal gives them
        // and therefore what their configuration expects.
        cmd.arg("-l");
        // Tell the shell what it is talking to. Without this it assumes a
        // dumb terminal and turns colour and line editing off.
        cmd.env(
            "TERM",
            std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
        );

        let child = pair.slave.spawn_command(cmd).map_err(std::io::Error::other)?;
        let pid = child.process_id();
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let writer = pair.master.take_writer().map_err(std::io::Error::other)?;
        let mut reader = pair.master.try_clone_reader().map_err(std::io::Error::other)?;

        // The pty read is blocking, so it lives on its own thread and feeds
        // the parser. It ends when the shell closes the pty.
        let sink = Arc::clone(&parser);
        std::thread::Builder::new()
            .name("agnt-pty".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut p) = sink.lock() {
                                p.process(&buf[..n]);
                            }
                            on_output();
                        }
                    }
                }
            })?;

        Ok(Self {
            parser,
            writer,
            master: pair.master,
            child,
            pid,
            size: (rows, cols),
            program: program.to_path_buf(),
        })
    }

    /// Which shell is running, for the line shown while it is starting.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// How many rows of the shell's screen have anything in them.
    ///
    /// The text area is drawn this tall, so an idle shell is one line and a
    /// command's output grows the box to fit — the way the task prompt grows
    /// with the text typed into it. A terminal fills from the top, so this is
    /// the row the cursor is on, or the last row with content if a program
    /// has left the cursor above its own output.
    pub fn used_rows(&self) -> u16 {
        let Ok(parser) = self.parser.lock() else {
            return 1;
        };
        let screen = parser.screen();
        let (cursor_row, _) = screen.cursor_position();
        let written = screen.contents().lines().count() as u16;
        cursor_row.saturating_add(1).max(written).max(1).min(self.size.0)
    }

    /// The screen, for rendering. Held only as long as the draw takes.
    pub fn parser(&self) -> &Arc<Mutex<vt100::Parser>> {
        &self.parser
    }

    /// Whether the shell has exited. A shell the user typed `exit` into
    /// should not leave a dead screen in the text area.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if self.size == (rows, cols) || rows == 0 || cols == 0 {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Where the shell is now, read from the operating system rather than
    /// asked of the shell.
    ///
    /// Asking would mean writing a line of the shell's own syntax into its
    /// prompt, which would show up in what the user sees and would have to be
    /// spelled differently for every shell. The kernel already knows.
    pub fn cwd(&self) -> Option<PathBuf> {
        process_cwd(self.pid?)
    }
}

impl Drop for PtyShell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A process's working directory, however this platform exposes it.
#[cfg(target_os = "linux")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// macOS has no `/proc`, and `lsof` is the portable way to ask. The `-F`
/// output is line-oriented and stable: `n` prefixes the name.
#[cfg(target_os = "macos")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    let out = std::process::Command::new("lsof")
        .args(["-a", "-d", "cwd", "-p", &pid.to_string(), "-Fn"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n'))
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// Turn a key the list received into the bytes a terminal would have sent.
///
/// Everything typed in shell mode goes to the shell, so this has to cover
/// what a shell's line editor listens for — the arrows and Home/End that
/// move the cursor, the control characters that edit the line, and Tab,
/// which is the completion key and the reason for all of this.
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+A..Ctrl+Z and the handful of control characters that
                // follow them in the ASCII table.
                let b = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => (c as u8) - b'a' + 1,
                    ' ' | '@' => 0,
                    '[' => 27,
                    '\\' => 28,
                    ']' => 29,
                    '^' => 30,
                    '_' | '?' => 31,
                    _ => return None,
                };
                vec![b]
            } else {
                let mut b = [0u8; 4];
                c.encode_utf8(&mut b).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n @ 1..=4) => vec![0x1b, b'O', b'P' + (n - 1)],
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            format!("\x1b[{code}~").into_bytes()
        }
        _ => return None,
    };

    // Alt is sent as an escape prefix, which is what terminals do and what
    // shells read back as Meta.
    Some(if alt {
        let mut v = vec![0x1b];
        v.extend(bytes);
        v
    } else {
        bytes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// `$SHELL` is not the answer: a development shell rewrites it. The
    /// operating system's record of the user's shell is, and an explicit
    /// `AGNT_SHELL` beats both.
    #[test]
    fn the_shell_is_the_users_own_not_whatever_rewrote_shell() {
        let os = login_shell().expect("the OS should know this user's shell");
        assert!(os.is_absolute(), "{os:?}");

        // SAFETY: single-threaded test. `program()` is called nowhere else in
        // the suite — every other test is handed its shell — so nothing races
        // on this.
        unsafe { std::env::set_var("AGNT_SHELL", "/bin/sh") };
        assert_eq!(program(), PathBuf::from("/bin/sh"));
        unsafe { std::env::remove_var("AGNT_SHELL") };

        // With no override, `$SHELL` loses to the OS. This is the `nix
        // develop` case: it exports its own bash as `$SHELL`.
        unsafe { std::env::set_var("SHELL", "/definitely/not/the/users/shell") };
        assert_eq!(program(), os);
        unsafe { std::env::remove_var("SHELL") };
    }

    #[test]
    fn tab_reaches_the_shell_as_tab() {
        // The whole point of the pty: completion is the shell's, so Tab has
        // to arrive as Tab rather than being read by the list.
        assert_eq!(encode_key(key(KeyCode::Tab)), Some(b"\t".to_vec()));
    }

    #[test]
    fn text_and_control_characters_encode_as_a_terminal_would() {
        assert_eq!(encode_key(key(KeyCode::Char('a'))), Some(b"a".to_vec()));
        assert_eq!(encode_key(key(KeyCode::Enter)), Some(b"\r".to_vec()));
        assert_eq!(encode_key(key(KeyCode::Backspace)), Some(vec![0x7f]));
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![3])
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(vec![21])
        );
        // Multi-byte text survives, which matters for a Japanese commit
        // message typed at the prompt.
        assert_eq!(
            encode_key(key(KeyCode::Char('あ'))),
            Some("あ".as_bytes().to_vec())
        );
    }

    #[test]
    fn cursor_keys_encode_as_escape_sequences() {
        assert_eq!(encode_key(key(KeyCode::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(key(KeyCode::Left)), Some(b"\x1b[D".to_vec()));
        assert_eq!(encode_key(key(KeyCode::Home)), Some(b"\x1b[H".to_vec()));
        // Alt is an escape prefix.
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'b'])
        );
    }

    /// The real thing: start the user's shell on a pty, type into it, and
    /// read what it drew back off the screen.
    #[tokio::test]
    async fn a_real_shell_runs_on_the_pty_and_reports_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();

        let mut shell = match PtyShell::spawn(Path::new("/bin/sh"), dir.path(), 24, 80, || {}) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("no pty available here ({e}); skipping");
                return;
            }
        };

        // Wait for the shell to draw its prompt, then send a command.
        let screen_has = |shell: &PtyShell, needle: &str| {
            shell
                .parser()
                .lock()
                .map(|p| p.screen().contents().contains(needle))
                .unwrap_or(false)
        };
        for _ in 0..100 {
            if shell.cwd().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        shell.send(b"cd sub\r");
        let mut moved = None;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(cwd) = shell.cwd()
                && cwd.canonicalize().ok() == sub.canonicalize().ok()
            {
                moved = Some(cwd);
                break;
            }
        }
        assert!(
            moved.is_some(),
            "the shell did not move, or its cwd could not be read; screen was:\n{}",
            shell.parser().lock().unwrap().screen().contents()
        );

        // And what it printed is on the screen agnt draws.
        shell.send(b"echo pty-canary\r");
        let mut seen = false;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if screen_has(&shell, "pty-canary") {
                seen = true;
                break;
            }
        }
        assert!(
            seen,
            "output never reached the screen:\n{}",
            shell.parser().lock().unwrap().screen().contents()
        );
    }
}
