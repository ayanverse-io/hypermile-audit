//! # hm-core
//!
//! Shared Hypermile engine: discovers Claude Code transcripts, parses them
//! defensively, and attributes tokens to categories and projects. Consumed by
//! the `hypermile-audit` CLI and other Hypermile tools.
//!
//! This crate performs **no network I/O, no telemetry, and contains no unsafe
//! code** (`unsafe_code = "forbid"` in `Cargo.toml`). It only reads files under
//! the provided claude directory.
//!
//! ## Quick start
//! ```no_run
//! let result = hm_core::scan(&hm_core::ScanOptions::default())?;
//! println!("{result:#?}");
//! # Ok::<(), hm_core::ScanError>(())
//! ```

mod categorize;
mod discover;
mod estimate;
mod model;
mod scan;
mod waste;

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use thiserror::Error;

use model::TranscriptLine;

// Re-export the pieces of the model useful to consumers/tests.
pub use categorize::Category;
pub use estimate::{estimate_tokens, looks_like_code_or_json};
pub use waste::{
    BlobKind, BlobSample, HugeOutput, JsonBlobs, LogNoise, NoiseSample, ReadOffender,
    RepeatedReads, SavingsRange, SubAgentShare, WasteReport,
};

/// Centralized brand string (spec 00 §1: must live in one constant per app so
/// it can be swapped).
pub const BRAND: &str = "Hypermile";

/// Production base URL. The audit report's download link is built from this
/// (spec 01 §4, retargeted to /download per strategy REV B).
pub const BASE_URL: &str = "https://hypermile.dev";

/// Default scan window in days (spec 01 §2: "last 7 days").
pub const DEFAULT_WINDOW_DAYS: u32 = 7;

/// Errors returned by [`scan`]. Discovery of a missing claude dir is **not** an
/// error — it yields an empty [`ScanResult`].
#[derive(Debug, Error)]
pub enum ScanError {
    /// The platform home directory could not be determined and no explicit
    /// `claude_dir` was supplied.
    #[error("could not determine home directory; pass an explicit claude_dir")]
    NoHomeDir,
}

/// Inputs to a scan. Construct via [`ScanOptions::default`] and override fields,
/// or build directly. `claude_dir` is always injectable so tests (and the
/// `--claude-dir` flag) can point at fixtures.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Override for the `.claude` directory. `None` = platform home `.claude`.
    pub claude_dir: Option<PathBuf>,
    /// Look back this many days by file mtime. `0` means "no lower bound".
    pub days: u32,
    /// Limit to project dirs whose name/path matches this substring or `*`-glob.
    pub project_filter: Option<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            claude_dir: None,
            days: DEFAULT_WINDOW_DAYS,
            project_filter: None,
        }
    }
}

/// Headline totals (spec 01 §4). `tokens` is the real "total context processed"
/// figure from API `usage`; `cache_read` is the cache-read subset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    pub tokens: u64,
    pub cache_read: u64,
    pub sessions: usize,
    pub projects: usize,
}

/// Estimated tokens attributed to one [`Category`] (chars-heuristic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryTokens {
    pub category: Category,
    pub tokens: u64,
}

/// Per-project rollup. `tokens` uses real API `usage` so project figures sum to
/// [`Totals::tokens`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectBreakdown {
    /// The sanitized `projects/<name>` directory name.
    pub name: String,
    pub tokens: u64,
    pub sessions: usize,
}

/// The complete result of a scan. Designed to serialize cleanly into the CLI's
/// `--json` schema (spec 01 §4) and to feed the desktop analytics view.
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    /// The window actually scanned, echoed back for the report header.
    pub window_days: u32,
    pub totals: Totals,
    /// Categories in stable [`Category::DISPLAY_ORDER`], zero-token categories
    /// omitted.
    pub categories: Vec<CategoryTokens>,
    /// Projects sorted by descending token usage.
    pub projects: Vec<ProjectBreakdown>,
    /// Count of transcript lines that failed to parse across all files.
    pub parse_errors: u64,
}

impl ScanResult {
    /// Total estimated tokens across all categories (denominator for percentages).
    pub fn category_total(&self) -> u64 {
        self.categories.iter().map(|c| c.tokens).sum()
    }

    /// Percentage of estimated tokens in `category` (0.0 when nothing scanned).
    pub fn category_pct(&self, category: Category) -> f64 {
        let total = self.category_total();
        if total == 0 {
            return 0.0;
        }
        let tokens = self
            .categories
            .iter()
            .find(|c| c.category == category)
            .map(|c| c.tokens)
            .unwrap_or(0);
        (tokens as f64 / total as f64) * 100.0
    }

    /// True when no transcript files were found (drives the CLI's friendly
    /// "no transcripts found" message).
    pub fn is_empty(&self) -> bool {
        self.totals.sessions == 0
    }
}

/// Run a scan (spec 01 §3 steps 1–3: discover → parse → attribute).
///
/// Returns [`ScanError::NoHomeDir`] only when no `claude_dir` is supplied and
/// the home directory cannot be determined. A valid-but-absent claude dir
/// yields an empty [`ScanResult`] (not an error).
pub fn scan(options: &ScanOptions) -> Result<ScanResult, ScanError> {
    let claude_dir = discover::resolve_claude_dir(options.claude_dir.as_deref())
        .ok_or(ScanError::NoHomeDir)?;

    Ok(scan_dir(&claude_dir, options.days, options.project_filter.as_deref()))
}

/// Core scan against an explicit directory (infallible: missing dir → empty).
fn scan_dir(claude_dir: &Path, days: u32, project_filter: Option<&str>) -> ScanResult {
    let files = discover::discover(claude_dir, days, project_filter);

    let mut totals = Totals::default();
    let mut category_tokens: HashMap<Category, u64> = HashMap::new();
    let mut project_tokens: HashMap<String, (u64, usize)> = HashMap::new();
    let mut parse_errors: u64 = 0;

    for file in &files {
        let fs = scan::scan_file(&file.path);

        totals.tokens += fs.usage_total;
        totals.cache_read += fs.cache_read;
        parse_errors += fs.parse_errors;

        for (cat, toks) in fs.categories {
            *category_tokens.entry(cat).or_insert(0) += toks;
        }

        let entry = project_tokens.entry(file.project.clone()).or_insert((0, 0));
        entry.0 += fs.usage_total;
        entry.1 += 1;
    }

    totals.sessions = files.len();
    totals.projects = project_tokens.len();

    // Categories in stable display order, dropping empties.
    let categories = Category::DISPLAY_ORDER
        .iter()
        .filter_map(|&cat| {
            category_tokens.get(&cat).map(|&tokens| CategoryTokens {
                category: cat,
                tokens,
            })
        })
        .collect();

    // Projects sorted by tokens desc, then name for stable ordering.
    let mut projects: Vec<ProjectBreakdown> = project_tokens
        .into_iter()
        .map(|(name, (tokens, sessions))| ProjectBreakdown {
            name,
            tokens,
            sessions,
        })
        .collect();
    projects.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.name.cmp(&b.name)));

    ScanResult {
        window_days: days,
        totals,
        categories,
        projects,
        parse_errors,
    }
}

/// Run the waste analysis (spec 01 §3.4–3.5) over the same transcripts
/// [`scan`] sees.
///
/// This is a second streamed pass, independent of [`scan`], so `scan`'s public
/// contract is unchanged. Like [`scan`], a missing claude dir is not an error —
/// it yields an all-zero [`WasteReport`] whose [`SubAgentShare`] is
/// [`SubAgentShare::Unknown`] and whose savings range is `0–0 %`.
///
/// Returns [`ScanError::NoHomeDir`] only when no `claude_dir` is supplied and
/// the home directory cannot be determined.
pub fn analyze_waste(options: &ScanOptions) -> Result<WasteReport, ScanError> {
    let claude_dir = discover::resolve_claude_dir(options.claude_dir.as_deref())
        .ok_or(ScanError::NoHomeDir)?;

    Ok(analyze_dir(
        &claude_dir,
        options.days,
        options.project_filter.as_deref(),
    ))
}

/// Core waste pass against an explicit directory (infallible: missing dir →
/// empty report). File reading lives here; the detectors in [`waste`] stay pure
/// over parsed [`TranscriptLine`] values.
fn analyze_dir(claude_dir: &Path, days: u32, project_filter: Option<&str>) -> WasteReport {
    let files = discover::discover(claude_dir, days, project_filter);
    let mut acc = waste::WasteAccumulator::new();

    for file in &files {
        let mut session = waste::SessionWaste::new(file.project.clone());
        if let Ok(handle) = File::open(&file.path) {
            for line in BufReader::new(handle).lines() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(parsed) = serde_json::from_str::<TranscriptLine>(&line) {
                    session.observe(&parsed);
                }
            }
        }
        acc.add_session(session);
    }

    acc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_claude_dir_yields_empty_result() {
        let opts = ScanOptions {
            claude_dir: Some(PathBuf::from("this/does/not/exist/anywhere")),
            days: 7,
            project_filter: None,
        };
        let result = scan(&opts).unwrap();
        assert!(result.is_empty());
        assert_eq!(result.totals.sessions, 0);
        assert_eq!(result.parse_errors, 0);
    }

    #[test]
    fn default_options_have_seven_day_window() {
        assert_eq!(ScanOptions::default().days, DEFAULT_WINDOW_DAYS);
    }

    #[test]
    fn analyze_waste_missing_dir_is_all_zero() {
        let opts = ScanOptions {
            claude_dir: Some(PathBuf::from("this/does/not/exist/anywhere")),
            days: 7,
            project_filter: None,
        };
        let report = analyze_waste(&opts).unwrap();
        assert_eq!(report.repeated_reads.wasted_tokens, 0);
        assert_eq!(report.json_blobs.count, 0);
        assert_eq!(report.log_noise.count, 0);
        assert!(report.huge_outputs.is_empty());
        assert_eq!(report.sub_agent, SubAgentShare::Unknown);
        assert_eq!(report.savings.low, 0.0);
        assert_eq!(report.savings.high, 0.0);
    }

    #[test]
    fn category_pct_sums_to_100_when_nonempty() {
        let result = ScanResult {
            window_days: 7,
            totals: Totals::default(),
            categories: vec![
                CategoryTokens { category: Category::BashOutput, tokens: 30 },
                CategoryTokens { category: Category::FileReads, tokens: 70 },
            ],
            projects: vec![],
            parse_errors: 0,
        };
        let sum = result.category_pct(Category::BashOutput)
            + result.category_pct(Category::FileReads);
        assert!((sum - 100.0).abs() < 1e-9);
    }
}
