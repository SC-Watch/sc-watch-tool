//! The terminal (watch.Console): announcements scroll, and one status line
//! sits below them, redrawn in place - without it the console is silent
//! whenever nothing new appears, which looks exactly like a hang.
//!
//! The status is clamped to the terminal's width: a carriage return only
//! returns to the start of the current row, so a line that wraps would
//! march down the screen one row per redraw. Redirected output gets the
//! announcements only.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;

use crate::sinks::Console;

struct State {
    out: Box<dyn Write + Send>,
    /// Characters of status on screen now.
    width: usize,
    last_status: String,
}

pub struct Terminal {
    live: bool,
    columns: Box<dyn Fn() -> usize + Send + Sync>,
    state: Mutex<State>,
}

/// The terminal's width in columns: COLUMNS, then the console's own, then
/// 100 (shutil.get_terminal_size).
pub fn terminal_columns() -> usize {
    if let Some(n) = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
    {
        return n;
    }
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .filter(|w| *w > 0)
        .unwrap_or(100)
}

impl Terminal {
    /// Standard output, live when it is a terminal.
    pub fn stdout() -> Self {
        Self::new(
            Box::new(std::io::stdout()),
            std::io::stdout().is_terminal(),
            Box::new(terminal_columns),
        )
    }

    pub fn new(
        out: Box<dyn Write + Send>,
        live: bool,
        columns: Box<dyn Fn() -> usize + Send + Sync>,
    ) -> Self {
        Terminal {
            live,
            columns,
            state: Mutex::new(State {
                out,
                width: 0,
                last_status: String::new(),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn clear(&self, s: &mut State) {
        if self.live && s.width > 0 {
            let blank = format!("\r{}\r", " ".repeat(s.width));
            let _ = s.out.write_all(blank.as_bytes());
            s.width = 0;
        }
    }

    /// A line, with the status redrawn under it.
    fn line(&self, text: &str) {
        let mut s = self.state();
        self.clear(&mut s);
        let _ = writeln!(s.out, "{text}");
        if self.live && !s.last_status.is_empty() {
            let last = s.last_status.clone();
            let _ = s.out.write_all(last.as_bytes());
            s.width = last.chars().count();
        }
        let _ = s.out.flush();
    }
}

impl Console for Terminal {
    fn status(&self, text: &str) {
        if !self.live {
            return;
        }
        let width = (self.columns)().saturating_sub(1);
        let text: String = if text.chars().count() > width {
            let keep: String = text.chars().take(width.saturating_sub(1)).collect();
            format!("{keep}\u{2026}")
        } else {
            text.to_string()
        };
        let mut s = self.state();
        s.last_status = text.clone();
        self.clear(&mut s);
        let _ = s.out.write_all(text.as_bytes());
        let _ = s.out.flush();
        s.width = text.chars().count();
    }

    fn say(&self, text: &str) {
        self.line(text);
    }

    fn done(&self) {
        let mut s = self.state();
        self.clear(&mut s);
        let _ = s.out.flush();
    }

    /// As an announcement: the Python's print() wrote over the status line.
    fn print(&self, text: &str) {
        self.line(text);
    }

    fn eprint(&self, text: &str) {
        {
            let mut s = self.state();
            self.clear(&mut s);
            let _ = s.out.flush();
        }
        eprintln!("{text}");
    }
}
