//! `hypermile-audit` — Phase 0 lead-magnet CLI (spec 01).
//!
//! Scans local Claude Code transcripts via `hm_core`, categorizes token usage,
//! flags waste, and estimates savings. Local-only, zero network, zero telemetry.
//!
//! Output modes (spec 01 §2/§4):
//!   * default  → colored terminal report (plain when piped or `NO_COLOR`)
//!   * `--json` → stable machine-readable schema on stdout (nothing else)
//!   * `--html` → a self-contained shareable HTML file
//!
//! Arg parsing is hand-rolled (see [`cli`]) to keep the release binary small.

mod cli;
mod html;
mod json;
mod render;
mod summary;
mod terminal;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use hm_core::{analyze_waste, scan, ScanOptions};

use cli::{Action, Cli};

fn main() -> ExitCode {
    // Skip argv[0] (program name).
    let action = cli::parse(std::env::args().skip(1));
    match action {
        Action::Help => {
            println!("{}", cli::help_text());
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("hypermile-audit {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Action::Error(msg) => {
            eprintln!("error: {msg}");
            eprintln!("try 'hypermile-audit --help'");
            // Conventional exit code for a usage error.
            ExitCode::from(2)
        }
        Action::Run(cli) => match run(&cli) {
            Ok(code) => code,
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Execute a scan for the parsed [`Cli`]. Returns the process exit code, or an
/// error message for the fatal (undetermined home dir) case.
fn run(cli: &Cli) -> Result<ExitCode, String> {
    let options = ScanOptions {
        claude_dir: cli.claude_dir.clone(),
        days: cli.days,
        project_filter: cli.project.clone(),
    };

    let result = scan(&options).map_err(|e| e.to_string())?;
    let waste = analyze_waste(&options).map_err(|e| e.to_string())?;

    // HTML is a side artifact and may be combined with any stdout mode.
    if let Some(path) = &cli.html {
        let doc = html::render(&result, &waste, cli.redact_paths);
        std::fs::write(path, doc).map_err(|e| format!("writing {}: {e}", path.display()))?;
        // Status to stderr so `--json` keeps stdout clean.
        eprintln!("Wrote HTML report to {}", path.display());
    }

    if cli.json {
        // `--json`: exactly the schema on stdout, nothing else (even when empty).
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{}", json::render(&result, &waste, cli.redact_paths));
        return Ok(ExitCode::SUCCESS);
    }

    // Terminal mode.
    let stdout_is_tty = std::io::stdout().is_terminal();
    let text = if result.is_empty() {
        terminal::empty_message(result.window_days)
    } else {
        terminal::render(&result, &waste, cli.redact_paths, stdout_is_tty)
    };
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}
