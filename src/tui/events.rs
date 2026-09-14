//! Keyboard input, delivered asynchronously.
//!
//! crossterm's event API is blocking. Rather than pull in its `event-stream`
//! feature and a futures dependency, one OS thread polls and forwards over a
//! tokio channel, which keeps the render loop free to also await health
//! checks and timers.

use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use std::time::Duration;
use tokio::sync::mpsc;

/// What a keypress means to the launcher, independent of which screen is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Select,
    Refresh,
    ToggleLogs,
    Quit,
    /// Ctrl-C. Distinct from Quit so a launch in progress can report that it
    /// was interrupted rather than simply closing.
    Interrupt,
    /// A digit key, used to pick from a numbered list.
    Digit(u8),
    Other,
}

pub fn spawn_reader() -> (mpsc::Receiver<Action>, InputHandle) {
    let (tx, rx) = mpsc::channel(32);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    let handle = std::thread::spawn(move || loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        match crossterm::event::poll(Duration::from_millis(100)) {
            Ok(true) => match crossterm::event::read() {
                Ok(Event::Key(key)) => {
                    if let Some(action) = classify(key) {
                        if tx.blocking_send(action).is_err() {
                            break;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    });

    (
        rx,
        InputHandle {
            stop: stop_tx,
            handle: Some(handle),
        },
    )
}

/// Stops the reader thread on drop so the terminal is not left with a thread
/// competing for stdin after the TUI closes — which would otherwise swallow
/// the first keystrokes codex-local expects.
pub struct InputHandle {
    stop: std::sync::mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for InputHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn classify(key: KeyEvent) -> Option<Action> {
    // Windows terminals emit both Press and Release; only act on Press.
    if key.kind == KeyEventKind::Release {
        return None;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') => Some(Action::Interrupt),
            KeyCode::Char('n') => Some(Action::Down),
            KeyCode::Char('p') => Some(Action::Up),
            _ => Some(Action::Other),
        };
    }

    Some(match key.code {
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Enter => Action::Select,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('l') => Action::ToggleLogs,
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Char(c @ '1'..='9') => Action::Digit(c as u8 - b'0'),
        _ => Action::Other,
    })
}
