//! Interactive prompts. All are no-ops in non-interactive contexts (piped,
//! `--yes`, no TTY) so the CLI never blocks a script waiting on input.

use std::io::{BufRead, Write};

use crate::output::Ctx;

/// Parse a yes/no reply, falling back to `default` on empty/garbage.
pub fn parse_yes_no(input: &str, default: bool) -> bool {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}

/// Ask a yes/no question. Non-interactive returns `assume_yes || default`.
pub fn confirm(ctx: &Ctx, question: &str, default: bool) -> bool {
    if ctx.assume_yes {
        return true;
    }
    if !ctx.interactive {
        return default;
    }
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    print!("{question} {hint} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return default;
    }
    parse_yes_no(&line, default)
}

/// Pick one of `items`. Returns `None` when non-interactive or on invalid
/// input. `prompt` is shown above the numbered list.
pub fn select(ctx: &Ctx, prompt: &str, items: &[String]) -> Option<usize> {
    if !ctx.interactive || items.is_empty() {
        return None;
    }
    ctx.line(prompt);
    for (i, item) in items.iter().enumerate() {
        ctx.line(&format!("  {}. {}", i + 1, item));
    }
    print!("> ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let n: usize = line.trim().parse().ok()?;
    if n >= 1 && n <= items.len() {
        Some(n - 1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_no_parsing() {
        assert!(parse_yes_no("y", false));
        assert!(parse_yes_no("YES", false));
        assert!(!parse_yes_no("n", true));
        assert!(!parse_yes_no("no", true));
        assert!(parse_yes_no("", true)); // empty → default
        assert!(!parse_yes_no("garbage", false)); // invalid → default
    }
}
