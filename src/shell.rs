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
    choose_shell(
        std::env::var_os("AGNT_SHELL"),
        login_shell(),
        std::env::var_os("SHELL"),
    )
}

/// The choice itself, given the three answers.
///
/// Separated from where the answers come from so a test can make the choice
/// without rewriting the environment of the whole process — which is shared
/// with every other test running beside it.
fn choose_shell(
    explicit: Option<std::ffi::OsString>,
    login: Option<PathBuf>,
    env: Option<std::ffi::OsString>,
) -> PathBuf {
    let named = |v: Option<std::ffi::OsString>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
    named(explicit)
        .or(login)
        .or_else(|| named(env))
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

/// The answers a terminal owes the program running on it.
///
/// A shell asks the terminal what it is and waits for the reply before
/// drawing anything. fish sends a Primary Device Attributes query and waits
/// **ten seconds** for it, then gives up with a warning — which is exactly
/// what agnt used to do to it, because `vt100` interprets these sequences but
/// never answers them, and nothing else was writing back either.
///
/// The queries come in a set with Primary DA last, and a reply to that one is
/// the signal that every earlier query has been answered as far as it is
/// going to be. So answering DA promptly is what unblocks the shell; the
/// others are answered because a program that asks deserves an answer, not
/// because anything waits on them.
///
/// What the answers claim is deliberately modest — a VT100 with the advanced
/// video option, which is close to what `vt100` emulates. Claiming more would
/// invite a program to send sequences the emulator would then mangle.
#[derive(Debug, Default)]
pub struct QueryResponder {
    /// An escape sequence split across two reads, held until it is whole.
    pending: Vec<u8>,
}

/// A partial sequence longer than this is not a sequence. Without a bound,
/// output that happens to contain a stray `ESC` would grow this forever.
const MAX_PENDING: usize = 1024;

impl QueryResponder {
    /// Look through what the shell just wrote and answer anything it asked.
    ///
    /// `cursor` is where the cursor is *after* this output was processed,
    /// because a cursor position report has to describe the screen the shell
    /// will next draw on.
    pub fn scan(&mut self, chunk: &[u8], cursor: (u16, u16)) -> Vec<u8> {
        // Almost every read is plain output with no escape in it, and this
        // runs on the thread draining the pty, so the common case copies
        // nothing: the chunk is scanned where it lies. Only a sequence split
        // across two reads needs the two joined.
        let joined: Vec<u8>;
        let buf: &[u8] = if self.pending.is_empty() {
            chunk
        } else {
            let mut v = std::mem::take(&mut self.pending);
            v.extend_from_slice(chunk);
            joined = v;
            &joined
        };

        let mut out = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                i += 1;
                continue;
            }
            match parse_sequence(&buf[i..]) {
                Sequence::Incomplete => {
                    // Keep the tail so the sequence can be recognised once
                    // the rest of it arrives.
                    if buf.len() - i <= MAX_PENDING {
                        self.pending = buf[i..].to_vec();
                    }
                    return out;
                }
                Sequence::Complete { len, reply } => {
                    if let Some(reply) = reply {
                        out.extend_from_slice(&reply.bytes(cursor));
                    }
                    i += len;
                }
            }
        }
        out
    }
}

/// What a query wants back.
#[derive(Debug, PartialEq, Eq)]
enum Reply {
    /// Primary Device Attributes: what kind of terminal this is.
    PrimaryDa,
    /// Secondary Device Attributes: which firmware, nominally.
    SecondaryDa,
    /// Device status: still here.
    Ok,
    /// Where the cursor is, one-based.
    CursorPosition,
    /// The kitty keyboard protocol, none of whose modes are supported.
    KittyKeyboard,
    /// XTVERSION: the terminal's name.
    Version,
    /// A terminfo capability query that cannot be answered.
    NoCapability,
}

impl Reply {
    fn bytes(&self, (row, col): (u16, u16)) -> Vec<u8> {
        match self {
            Self::PrimaryDa => b"\x1b[?1;2c".to_vec(),
            Self::SecondaryDa => b"\x1b[>0;10;1c".to_vec(),
            Self::Ok => b"\x1b[0n".to_vec(),
            Self::CursorPosition => format!("\x1b[{};{}R", row + 1, col + 1).into_bytes(),
            Self::KittyKeyboard => b"\x1b[?0u".to_vec(),
            Self::Version => b"\x1bP>|agnt\x1b\\".to_vec(),
            Self::NoCapability => b"\x1bP0+r\x1b\\".to_vec(),
        }
    }
}

enum Sequence {
    /// A whole sequence, and what it asks for. Every sequence that is not a
    /// question is `Complete` with no reply — its length is what matters, so
    /// the scan resumes after it rather than inside it.
    Complete { len: usize, reply: Option<Reply> },
    /// The start of one; the rest has not arrived.
    Incomplete,
}

/// Recognise one escape sequence at the start of `s`, which begins with ESC.
///
/// This is not a terminal parser — `vt100` is doing that. It only needs to
/// find where each sequence ends, so that the few that are questions can be
/// answered and everything else skipped without losing the place.
fn parse_sequence(s: &[u8]) -> Sequence {
    match s.get(1) {
        None => Sequence::Incomplete,
        // CSI: parameters, then a final byte in 0x40..=0x7e.
        Some(b'[') => {
            let params_at = 2;
            let mut i = params_at;
            while i < s.len() {
                if (0x40..=0x7e).contains(&s[i]) {
                    let params = &s[params_at..i];
                    return Sequence::Complete {
                        len: i + 1,
                        reply: csi_reply(params, s[i]),
                    };
                }
                i += 1;
            }
            Sequence::Incomplete
        }
        // The string-terminated kinds: DCS, OSC, and the three — APC, PM,
        // SOS — that carry no meaning here but end the same way. They are
        // grouped rather than listed against a two-byte fallback, because a
        // fallback that guessed the wrong length would resume the scan in
        // the middle of a sequence and could answer something the shell
        // never asked.
        //
        // Only one of them is a question: a terminfo capability query, which
        // is answered with a refusal. The colour queries are deliberately
        // left alone — the reply would have to invent a colour, and a shell
        // themes itself from it, so a wrong answer is worse than none.
        Some(b'P' | b']' | b'_' | b'^' | b'X') => match find_terminator(s) {
            Some(end) => Sequence::Complete {
                len: end,
                reply: s
                    .get(2..4)
                    .filter(|head| s[1] == b'P' && *head == b"+q")
                    .map(|_| Reply::NoCapability),
            },
            None => Sequence::Incomplete,
        },
        // An escape with intermediate bytes before its final byte — a
        // charset designation such as `ESC ( B` is the common one.
        Some(0x20..=0x2f) => match s.iter().skip(1).position(|b| !(0x20..=0x2f).contains(b)) {
            Some(at) => Sequence::Complete {
                len: at + 2,
                reply: None,
            },
            None => Sequence::Incomplete,
        },
        // Everything else is two bytes, and none of them is a question.
        Some(_) => Sequence::Complete { len: 2, reply: None },
    }
}

/// The end of a string-terminated sequence, as a length.
///
/// One pass for whichever terminator arrives first: OSC may end with either
/// a BEL or an ST, and looking for one and then the other walked the buffer
/// twice to answer the same question.
fn find_terminator(s: &[u8]) -> Option<usize> {
    for (i, b) in s.iter().enumerate().skip(2) {
        if *b == 0x07 {
            return Some(i + 1);
        }
        if *b == b'\\' && s[i - 1] == 0x1b {
            return Some(i + 1);
        }
    }
    None
}

/// Which question a CSI sequence is asking, if it is asking one.
fn csi_reply(params: &[u8], final_byte: u8) -> Option<Reply> {
    match (params, final_byte) {
        (b"" | b"0", b'c') => Some(Reply::PrimaryDa),
        (b">" | b">0", b'c') => Some(Reply::SecondaryDa),
        (b"5", b'n') => Some(Reply::Ok),
        (b"6", b'n') => Some(Reply::CursorPosition),
        (b"?", b'u') => Some(Reply::KittyKeyboard),
        (b">" | b">0" | b">1", b'q') => Some(Reply::Version),
        _ => None,
    }
}

/// A running shell on a pseudo-terminal, and the screen it has drawn.
pub struct PtyShell {
    /// The parsed screen. The reader thread writes, the UI reads.
    parser: Arc<Mutex<vt100::Parser>>,
    /// Shared with the reader thread, which answers the shell's questions
    /// about the terminal as they arrive.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
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
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pair.master.take_writer().map_err(std::io::Error::other)?,
        ));
        let mut reader = pair.master.try_clone_reader().map_err(std::io::Error::other)?;

        // The pty read is blocking, so it lives on its own thread and feeds
        // the parser. It ends when the shell closes the pty.
        let sink = Arc::clone(&parser);
        let back = Arc::clone(&writer);
        std::thread::Builder::new()
            .name("agnt-pty".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                let mut responder = QueryResponder::default();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            // Process first, then answer: a cursor position
                            // report has to describe the screen as it is
                            // after this output, not before it.
                            let cursor = match sink.lock() {
                                Ok(mut p) => {
                                    p.process(&buf[..n]);
                                    p.screen().cursor_position()
                                }
                                Err(_) => (0, 0),
                            };
                            let reply = responder.scan(&buf[..n], cursor);
                            if !reply.is_empty()
                                && let Ok(mut w) = back.lock()
                            {
                                let _ = w.write_all(&reply);
                                let _ = w.flush();
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

    /// How many rows of the shell's screen have anything in them, and `0`
    /// when it has not drawn yet.
    ///
    /// The text area is drawn this tall, so an idle shell is one line and a
    /// command's output grows the box to fit — the way the task prompt grows
    /// with the text typed into it. A terminal fills from the top, so this is
    /// the row the cursor is on, or the last row with content if a program
    /// has left the cursor above its own output.
    ///
    /// Zero is a state the drawing cares about: a shell that has written
    /// nothing is still starting, and says so instead of showing an empty
    /// box. Answering both questions here means the screen's text is built
    /// once per frame rather than once for the height and again for the
    /// emptiness — `vt100` rebuilds it on every call.
    pub fn drawn_rows(&self) -> u16 {
        let Ok(parser) = self.parser.lock() else {
            return 0;
        };
        let screen = parser.screen();
        let written = screen.contents().lines().count() as u16;
        if written == 0 {
            return 0;
        }
        let (cursor_row, _) = screen.cursor_position();
        cursor_row.saturating_add(1).max(written).min(self.size.0)
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
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
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

        let some = |s: &str| Some(std::ffi::OsString::from(s));
        let wrong = some("/definitely/not/the/users/shell");

        // An explicit choice beats everything.
        assert_eq!(
            choose_shell(some("/bin/sh"), Some(os.clone()), wrong.clone()),
            PathBuf::from("/bin/sh")
        );
        // With no override, `$SHELL` loses to the operating system. This is
        // the `nix develop` case: it exports its own bash as `$SHELL`.
        assert_eq!(choose_shell(None, Some(os.clone()), wrong.clone()), os);
        // `$SHELL` is still better than nothing.
        assert_eq!(
            choose_shell(None, None, some("/bin/zsh")),
            PathBuf::from("/bin/zsh")
        );
        // An empty variable is not an answer.
        assert_eq!(choose_shell(some(""), None, None), PathBuf::from("/bin/sh"));
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

    /// The query fish blocks on. Ten seconds of agnt's startup was this one
    /// going unanswered.
    #[test]
    fn a_primary_device_attributes_query_is_answered() {
        let mut r = QueryResponder::default();
        assert_eq!(r.scan(b"\x1b[0c", (0, 0)), b"\x1b[?1;2c".to_vec());
        assert_eq!(r.scan(b"\x1b[c", (0, 0)), b"\x1b[?1;2c".to_vec());
    }

    #[test]
    fn the_other_questions_a_shell_asks_are_answered_too() {
        let mut r = QueryResponder::default();
        assert_eq!(r.scan(b"\x1b[>c", (0, 0)), b"\x1b[>0;10;1c".to_vec());
        assert_eq!(r.scan(b"\x1b[5n", (0, 0)), b"\x1b[0n".to_vec());
        assert_eq!(r.scan(b"\x1b[?u", (0, 0)), b"\x1b[?0u".to_vec());
        assert_eq!(r.scan(b"\x1b[>0q", (0, 0)), b"\x1bP>|agnt\x1b\\".to_vec());
        // A terminfo capability query is refused rather than ignored, which
        // is an answer the asker knows what to do with.
        assert_eq!(
            r.scan(b"\x1bP+q696e646e\x1b\\", (0, 0)),
            b"\x1bP0+r\x1b\\".to_vec()
        );
    }

    /// A cursor position report describes the screen the shell is about to
    /// draw on, and is one-based where the emulator is zero-based.
    #[test]
    fn a_cursor_position_report_gives_the_position() {
        let mut r = QueryResponder::default();
        assert_eq!(r.scan(b"\x1b[6n", (4, 11)), b"\x1b[5;12R".to_vec());
    }

    /// Output arrives in whatever sized pieces the pty hands over, so a query
    /// can be split across two reads. Holding the tail is what makes the
    /// answer arrive at all.
    #[test]
    fn a_query_split_across_two_reads_is_still_answered() {
        let mut r = QueryResponder::default();
        assert!(r.scan(b"hello\x1b[", (0, 0)).is_empty());
        assert_eq!(r.scan(b"0c", (0, 0)), b"\x1b[?1;2c".to_vec());

        // And across three, with the escape alone at the end of one.
        let mut r = QueryResponder::default();
        assert!(r.scan(b"\x1b", (0, 0)).is_empty());
        assert!(r.scan(b"[", (0, 0)).is_empty());
        assert_eq!(r.scan(b"c", (0, 0)), b"\x1b[?1;2c".to_vec());
    }

    /// Ordinary output must not be mistaken for a question, and sequences
    /// that are not questions must not throw the scan off the ones that are.
    #[test]
    fn ordinary_output_is_not_mistaken_for_a_question() {
        let mut r = QueryResponder::default();
        assert!(r.scan(b"just some text\r\n", (0, 0)).is_empty());
        // Colour, alternate screen, a title, a two-byte escape: all skipped.
        assert!(
            r.scan(b"\x1b[0m\x1b[?1049h\x1b]0;a title\x07\x1bM\x1b[1;5H", (0, 0))
                .is_empty()
        );
        // Still finds a real query after all of that.
        assert_eq!(
            r.scan(b"\x1b[38;2;1;2;3mcoloured\x1b[0m\x1b[0c", (0, 0)),
            b"\x1b[?1;2c".to_vec()
        );
    }

    /// A sequence the scanner has no interest in still has to be measured
    /// correctly, or the scan resumes inside it and answers something the
    /// shell never asked.
    #[test]
    fn sequences_that_are_not_questions_are_still_framed() {
        let mut r = QueryResponder::default();
        // APC, PM and SOS end with ST like DCS and OSC do. Treating them as
        // two bytes would leave their payload to be scanned as output.
        for opener in [b'_', b'^', b'X'] {
            let mut seq = vec![0x1b, opener];
            seq.extend_from_slice(b"\x1b[0c pretending to be a query");
            seq.extend_from_slice(b"\x1b\\");
            assert!(
                r.scan(&seq, (0, 0)).is_empty(),
                "answered something inside a {opener} string"
            );
            // And the scanner is still in step afterwards.
            assert_eq!(r.scan(b"\x1b[0c", (0, 0)), b"\x1b[?1;2c".to_vec());
        }

        // An escape with intermediate bytes: `ESC ( B`, a charset choice, is
        // three bytes rather than two.
        assert!(r.scan(b"\x1b(B", (0, 0)).is_empty());
        assert_eq!(r.scan(b"\x1b[0c", (0, 0)), b"\x1b[?1;2c".to_vec());

        // An OSC ending in BEL and one ending in ST are both found, and the
        // query after either is answered.
        for terminator in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
            let mut seq = b"\x1b]0;a window title".to_vec();
            seq.extend_from_slice(terminator);
            seq.extend_from_slice(b"\x1b[0c");
            assert_eq!(r.scan(&seq, (0, 0)), b"\x1b[?1;2c".to_vec());
        }
    }

    /// A stray escape in the middle of output must not make the scanner hold
    /// bytes for ever waiting for a sequence that is never coming.
    ///
    /// The bytes here are digits, which are CSI *parameters*: they keep the
    /// sequence open, which is what a run of ordinary letters would not do —
    /// a letter is a final byte and ends it on the spot.
    #[test]
    fn a_stray_escape_does_not_grow_without_bound() {
        let mut r = QueryResponder::default();
        r.scan(b"\x1b[", (0, 0));
        assert_eq!(r.pending.len(), 2);

        r.scan(&vec![b'1'; MAX_PENDING / 2], (0, 0));
        assert!(!r.pending.is_empty(), "a real long sequence must be held");

        r.scan(&vec![b'1'; MAX_PENDING * 2], (0, 0));
        assert!(r.pending.is_empty(), "{} bytes held", r.pending.len());

        // And it recovers: the next real query is still answered.
        assert_eq!(r.scan(b"\x1b[0c", (0, 0)), b"\x1b[?1;2c".to_vec());
    }

    /// End to end: a real program on a real pty asks what the terminal is,
    /// and the answer comes back to it.
    ///
    /// The reply is written to the pty's input, where the tty driver echoes
    /// it — which is how it becomes visible on the screen agnt draws, and how
    /// this test can see that it was sent at all.
    #[tokio::test]
    async fn a_program_that_asks_what_the_terminal_is_gets_an_answer() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = match PtyShell::spawn(Path::new("/bin/sh"), dir.path(), 24, 80, || {}) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("no pty available here ({e}); skipping");
                return;
            }
        };

        let screen = |s: &PtyShell| {
            s.parser()
                .lock()
                .map(|p| p.screen().contents())
                .unwrap_or_default()
        };
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if !screen(&shell).trim().is_empty() {
                break;
            }
        }

        shell.send(b"printf '\\033[0c'\r");
        let mut answered = false;
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            // The parameters of the reply, whatever the echo does with the
            // escape byte in front of them.
            if screen(&shell).contains("[?1;2c") {
                answered = true;
                break;
            }
        }
        assert!(
            answered,
            "the device attributes query went unanswered; screen was:\n{}",
            screen(&shell)
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
