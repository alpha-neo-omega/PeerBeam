//! Output context + rendering. One place decides colour, JSON, interactivity,
//! and progress based on flags and whether we're on a terminal — so the CLI
//! is SSH/pipe-friendly by default (no colour or prompts when not a TTY).

use std::io::{IsTerminal, Write};

use crate::exit::CliError;

pub struct Ctx {
    pub json: bool,
    pub color: bool,
    pub color_err: bool,
    pub interactive: bool,
    pub progress: bool,
    pub verbose: u8,
    pub quiet: bool,
    pub assume_yes: bool,
}

impl Ctx {
    pub fn new(json: bool, no_color: bool, verbose: u8, quiet: bool, assume_yes: bool) -> Self {
        let stdout_tty = std::io::stdout().is_terminal();
        let stderr_tty = std::io::stderr().is_terminal();
        let stdin_tty = std::io::stdin().is_terminal();
        let dumb = std::env::var_os("NO_COLOR").is_some()
            || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);
        let allow_color = !no_color && !dumb;

        Ctx {
            json,
            color: allow_color && !json && stdout_tty,
            color_err: allow_color && stderr_tty,
            interactive: !json && !assume_yes && stdin_tty && stdout_tty,
            progress: !json && !quiet && stderr_tty,
            verbose,
            quiet,
            assume_yes,
        }
    }

    // ── Painting ─────────────────────────────────────────────
    fn wrap(on: bool, s: &str, code: &str) -> String {
        if on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        Self::wrap(self.color, s, "1")
    }
    pub fn dim(&self, s: &str) -> String {
        Self::wrap(self.color, s, "2")
    }
    pub fn green(&self, s: &str) -> String {
        Self::wrap(self.color, s, "32")
    }
    pub fn yellow(&self, s: &str) -> String {
        Self::wrap(self.color, s, "33")
    }
    pub fn red(&self, s: &str) -> String {
        Self::wrap(self.color, s, "31")
    }

    // ── Emit ─────────────────────────────────────────────────
    pub fn line(&self, s: &str) {
        if !self.quiet {
            println!("{s}");
        }
    }

    pub fn json_line(&self, value: &serde_json::Value) {
        println!("{}", serde_json::to_string(value).unwrap_or_default());
    }

    pub fn error(&self, e: &CliError) {
        let msg = format!("error: {e}");
        eprintln!("{}", Self::wrap(self.color_err, &msg, "31"));
    }

    /// A left-aligned column table (skipped in JSON mode — callers emit JSON).
    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) {
        let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                if i < widths.len() {
                    widths[i] = widths[i].max(cell.len());
                }
            }
        }
        let header: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| self.bold(&format!("{:<width$}", h, width = widths[i])))
            .collect();
        self.line(&header.join("  "));
        for row in rows {
            let line: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{:<width$}", c, width = widths.get(i).copied().unwrap_or(0)))
                .collect();
            self.line(&line.join("  "));
        }
    }

    /// A progress bar bound to stderr; a no-op when not attached to a terminal.
    pub fn bar(&self, total: u64, label: &str) -> Bar<'_> {
        Bar {
            ctx: self,
            total: total.max(1),
            label: label.to_string(),
        }
    }
}

pub struct Bar<'a> {
    ctx: &'a Ctx,
    total: u64,
    label: String,
}

impl Bar<'_> {
    pub fn update(&self, done: u64) {
        if !self.ctx.progress {
            return;
        }
        let frac = (done as f64 / self.total as f64).clamp(0.0, 1.0);
        let width = 28usize;
        let filled = (frac * width as f64).round() as usize;
        let bar: String = "█".repeat(filled) + &"░".repeat(width - filled);
        eprint!("\r{} [{}] {:>3}%", self.label, bar, (frac * 100.0) as u32);
        let _ = std::io::stderr().flush();
    }

    pub fn finish(&self) {
        if self.ctx.progress {
            eprintln!();
        }
    }
}
