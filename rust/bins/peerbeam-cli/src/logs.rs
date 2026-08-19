//! `peerbeam logs` — what the engine has been doing.
//!
//! Reads the log **file**, because a one-shot command's own in-memory ring is
//! empty by definition. Until this existed the engine captured logs that no
//! frontend could reach: the buffer was addressable from the FFI and nowhere
//! else, which made "export logs" a documented capability nobody could invoke.

use crate::cli::LogsArgs;
use crate::exit::{CliError, CliResult};
use crate::output::Ctx;

pub fn logs(ctx: &Ctx, args: LogsArgs, path_override: Option<&str>) -> CliResult {
    let config = crate::commands::load_config(path_override)?;
    if !config.log.to_file {
        // Said plainly. An empty list here would look like "nothing happened"
        // when the truth is "nothing was recorded".
        return Err(CliError::Usage(
            "log.to_file is off, so there is no log file to read — turn it on \
             and restart the engine"
                .into(),
        ));
    }
    let path = std::path::Path::new(&config.storage.data_directory)
        .join("logs")
        .join("peerbeam.jsonl");

    let limit = usize::try_from(args.limit).unwrap_or(100);
    let out = peerbeam_logs::read_file(&path, limit);
    let lines = out
        .get("logs")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default();

    if let Some(dest) = args.export {
        if lines.is_empty() {
            return Err(CliError::NotFound(format!(
                "no logs at {} to export",
                path.display()
            )));
        }
        let dest = std::path::PathBuf::from(dest);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CliError::Other(format!("creating {}: {e}", parent.display())))?;
        }
        std::fs::copy(&path, &dest)
            .map_err(|e| CliError::Other(format!("writing {}: {e}", dest.display())))?;
        if ctx.json {
            ctx.json_line(&serde_json::json!({
                "path": dest.to_string_lossy(),
                "count": lines.len(),
            }));
            return Ok(());
        }
        ctx.line(&format!(
            "logs copied to {}",
            ctx.bold(&dest.to_string_lossy())
        ));
        return Ok(());
    }

    if ctx.json {
        ctx.json_line(&out);
        return Ok(());
    }
    if lines.is_empty() {
        // An empty file and a broken command look identical otherwise.
        ctx.line(&ctx.dim(&format!("no log lines yet at {}", path.display())));
        return Ok(());
    }
    for line in &lines {
        let at = line.get("at").and_then(|v| v.as_str()).unwrap_or("");
        let level = line.get("level").and_then(|v| v.as_str()).unwrap_or("");
        let msg = line.get("message").and_then(|v| v.as_str()).unwrap_or("");
        ctx.line(&format!("{} {level:<5} {msg}", ctx.dim(at)));
    }
    Ok(())
}
