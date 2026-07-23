//! Hand-rolled argument parsing (spec 01 §2).
//!
//! We deliberately avoid `clap`: the flag set is small and stable, and a
//! hand-rolled parser keeps the release binary tiny (no proc-macro / builder
//! dependency) so the <10 MB budget (spec 01 §6) holds with comfortable margin.
//! Parsing is total — every branch either yields [`Cli`] or an [`Action`] that
//! prints help/version/an error and sets the exit code.

use std::path::PathBuf;

/// The parsed invocation once flags are resolved.
#[derive(Debug, Clone)]
pub struct Cli {
    /// Look-back window in days (`--days`, default [`hm_core::DEFAULT_WINDOW_DAYS`]).
    pub days: u32,
    /// Limit to project dirs matching this substring / `*`-glob (`--project`).
    pub project: Option<String>,
    /// Override for `~/.claude` (`--claude-dir`, also drives the test fixtures).
    pub claude_dir: Option<PathBuf>,
    /// Emit the machine-readable JSON schema to stdout (`--json`).
    pub json: bool,
    /// Write a self-contained HTML report to this path (`--html <path>`).
    pub html: Option<PathBuf>,
    /// Replace file paths with 8-char hashes in all output (`--redact-paths`).
    pub redact_paths: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Cli {
            days: hm_core::DEFAULT_WINDOW_DAYS,
            project: None,
            claude_dir: None,
            json: false,
            html: None,
            redact_paths: false,
        }
    }
}

/// The outcome of parsing argv.
#[derive(Debug)]
pub enum Action {
    /// Run a scan with these options.
    Run(Box<Cli>),
    /// Print help text and exit 0.
    Help,
    /// Print the version line and exit 0.
    Version,
    /// A usage error: print the message to stderr and exit 2.
    Error(String),
}

const HELP: &str = "\
hypermile-audit — see where your Claude Code tokens go (local-only, zero network)

USAGE:
    hypermile-audit [OPTIONS]

OPTIONS:
    --days <N>            Scan the last N days of transcripts (default: 7)
    --project <PATH>      Limit to one project dir (substring or *-glob match)
    --json               Emit machine-readable JSON to stdout (nothing else)
    --html <PATH>        Write a self-contained shareable HTML report to PATH
    --claude-dir <DIR>   Override the ~/.claude directory to scan
    --redact-paths       Replace file paths with 8-char hashes in all output
    -h, --help           Print this help
    -V, --version        Print version

Scans <claude-dir>/projects/**/*.jsonl, categorizes token usage, flags waste,
and estimates savings. No file contents or prompt text ever leave your machine.";

/// Parse an argument iterator (argv minus the program name) into an [`Action`].
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Action {
    let mut cli = Cli::default();
    let mut it = args.into_iter();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Action::Help,
            "-V" | "--version" => return Action::Version,
            "--json" => cli.json = true,
            "--redact-paths" => cli.redact_paths = true,
            "--days" => match it.next() {
                Some(v) => match v.parse::<u32>() {
                    Ok(n) => cli.days = n,
                    Err(_) => return Action::Error(format!("--days expects a number, got '{v}'")),
                },
                None => return Action::Error("--days requires a value".into()),
            },
            "--project" => match it.next() {
                Some(v) => cli.project = Some(v),
                None => return Action::Error("--project requires a value".into()),
            },
            "--html" => match it.next() {
                Some(v) => cli.html = Some(PathBuf::from(v)),
                None => return Action::Error("--html requires a path".into()),
            },
            "--claude-dir" => match it.next() {
                Some(v) => cli.claude_dir = Some(PathBuf::from(v)),
                None => return Action::Error("--claude-dir requires a path".into()),
            },
            // Support `--flag=value` forms too.
            other if other.starts_with("--days=") => {
                let v = &other["--days=".len()..];
                match v.parse::<u32>() {
                    Ok(n) => cli.days = n,
                    Err(_) => return Action::Error(format!("--days expects a number, got '{v}'")),
                }
            }
            other if other.starts_with("--project=") => {
                cli.project = Some(other["--project=".len()..].to_string());
            }
            other if other.starts_with("--html=") => {
                cli.html = Some(PathBuf::from(&other["--html=".len()..]));
            }
            other if other.starts_with("--claude-dir=") => {
                cli.claude_dir = Some(PathBuf::from(&other["--claude-dir=".len()..]));
            }
            other => return Action::Error(format!("unknown argument '{other}' (try --help)")),
        }
    }

    Action::Run(Box::new(cli))
}

/// The full help text (also used by the `--help` path in `main`).
pub fn help_text() -> &'static str {
    HELP
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Cli {
        match parse(args.iter().map(|s| s.to_string())) {
            Action::Run(c) => *c,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn defaults_are_seven_days_no_filters() {
        let c = parse_ok(&[]);
        assert_eq!(c.days, hm_core::DEFAULT_WINDOW_DAYS);
        assert!(c.project.is_none());
        assert!(!c.json && !c.redact_paths);
    }

    #[test]
    fn parses_all_flags() {
        let c = parse_ok(&[
            "--days", "30", "--project", "alpha", "--json", "--html", "out.html",
            "--claude-dir", "/tmp/c", "--redact-paths",
        ]);
        assert_eq!(c.days, 30);
        assert_eq!(c.project.as_deref(), Some("alpha"));
        assert!(c.json);
        assert_eq!(c.html.as_deref().unwrap().to_str().unwrap(), "out.html");
        assert_eq!(c.claude_dir.as_deref().unwrap().to_str().unwrap(), "/tmp/c");
        assert!(c.redact_paths);
    }

    #[test]
    fn equals_forms_parse() {
        let c = parse_ok(&["--days=14", "--project=beta", "--html=r.html"]);
        assert_eq!(c.days, 14);
        assert_eq!(c.project.as_deref(), Some("beta"));
        assert_eq!(c.html.as_deref().unwrap().to_str().unwrap(), "r.html");
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(parse(["--help".to_string()]), Action::Help));
        assert!(matches!(parse(["-h".to_string()]), Action::Help));
        assert!(matches!(parse(["--version".to_string()]), Action::Version));
        assert!(matches!(parse(["-V".to_string()]), Action::Version));
    }

    #[test]
    fn bad_days_is_an_error() {
        assert!(matches!(parse(["--days".to_string(), "abc".to_string()]), Action::Error(_)));
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(matches!(parse(["--nope".to_string()]), Action::Error(_)));
    }

    #[test]
    fn missing_value_is_an_error() {
        assert!(matches!(parse(["--days".to_string()]), Action::Error(_)));
    }
}
