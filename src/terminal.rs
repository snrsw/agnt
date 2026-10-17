//! Terminal ownership: entering/leaving the TUI, restoring the terminal on
//! panic, and handing the terminal to an official CLI and taking it back.

use std::io::{self, Stdout};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::providers::AttachPlan;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

static PANIC_HOOK: Once = Once::new();

/// Leave the alternate screen and raw mode. Safe to call more than once.
pub fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, cursor::Show)
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore();
            previous(info);
        }));
    });
}

/// Owns the terminal while the list is shown; restores it on drop.
pub struct TerminalGuard {
    terminal: Tui,
    active: bool,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok(Self {
            terminal,
            active: true,
        })
    }

    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }

    /// Give the terminal to a child: normal screen, cooked mode, cursor shown.
    pub fn release(&mut self) -> io::Result<()> {
        if self.active {
            restore()?;
            self.active = false;
        }
        Ok(())
    }

    /// Take the terminal back after a child exits and force a full redraw.
    pub fn reacquire(&mut self) -> io::Result<()> {
        if !self.active {
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen)?;
            self.active = true;
        }
        self.terminal.clear()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = restore();
        }
    }
}

/// Pauses the stdin reader while a child owns the terminal, so agnt does not
/// steal the child's keystrokes.
#[derive(Debug, Default)]
pub struct InputGate {
    paused: AtomicBool,
    /// Held by the reader for the duration of each poll/read; `pause` takes
    /// it once to be sure no read is in flight.
    io: Mutex<()>,
}

impl InputGate {
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        drop(self.io.lock().unwrap_or_else(|e| e.into_inner()));
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}

/// Blocking stdin reader on its own thread. Exits when the receiver is gone.
pub fn spawn_input_reader(gate: Arc<InputGate>, tx: mpsc::Sender<Event>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("agnt-input".into())
        .spawn(move || {
            loop {
                if tx.is_closed() {
                    return;
                }
                if gate.is_paused() {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                let _held = gate.io.lock().unwrap_or_else(|e| e.into_inner());
                if gate.is_paused() {
                    continue;
                }
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(ev) => {
                            if tx.blocking_send(ev).is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        })
        .expect("spawn input reader thread")
}

/// Run an attach/resume plan with the terminal handed over, then take it
/// back. The guard is restored even when the child fails to spawn.
pub async fn handoff(
    guard: &mut TerminalGuard,
    gate: &InputGate,
    plan: &AttachPlan,
) -> io::Result<ExitStatus> {
    gate.pause();
    guard.release()?;

    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args);
    if let Some(dir) = &plan.cwd {
        cmd.current_dir(dir);
    }
    let result = cmd.status().await;

    let back = guard.reacquire();
    gate.resume();
    back?;
    result
}

/// Run a plan without a TUI (CLI subcommands): inherit stdio, wait, return status.
pub async fn run_plan(plan: &AttachPlan) -> io::Result<ExitStatus> {
    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args);
    if let Some(dir) = &plan.cwd {
        cmd.current_dir(dir);
    }
    cmd.status().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_pause_waits_for_reader_to_leave_io() {
        let gate = Arc::new(InputGate::default());
        let held = gate.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _g = held.io.lock().unwrap();
            started_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(150));
        });
        started_rx.recv().unwrap();
        let t = std::time::Instant::now();
        gate.pause();
        assert!(t.elapsed() >= Duration::from_millis(100));
        assert!(gate.is_paused());
        gate.resume();
        assert!(!gate.is_paused());
        reader.join().unwrap();
    }
}
