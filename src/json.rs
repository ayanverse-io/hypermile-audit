//! The `--json` machine-readable report (spec 01 §4 schema, stable `schema: 1`).
//!
//! Shape:
//! ```json
//! {
//!   "schema": 1,
//!   "window_days": 7,
//!   "totals": { "tokens": 0, "cache_read": 0, "sessions": 0, "projects": 0 },
//!   "categories": [ { "name": "Bash output", "tokens": 0, "pct": 0.0 } ],
//!   "waste": [ { "kind": "repeated_reads", "tokens": 0, "detail": "…" } ],
//!   "savings_pct": { "low": 0.0, "high": 0.0 }
//! }
//! ```
//! All keys are snake_case; nothing else is written to stdout in `--json` mode.

use serde::Serialize;

use hm_core::{ScanResult, WasteReport};

use crate::render::round1;
use crate::summary::{findings, savings};

#[derive(Serialize)]
struct JsonReport {
    schema: u8,
    window_days: u32,
    totals: JsonTotals,
    categories: Vec<JsonCategory>,
    waste: Vec<JsonWaste>,
    savings_pct: JsonSavings,
}

#[derive(Serialize)]
struct JsonTotals {
    tokens: u64,
    cache_read: u64,
    sessions: usize,
    projects: usize,
}

#[derive(Serialize)]
struct JsonCategory {
    name: &'static str,
    tokens: u64,
    pct: f64,
}

#[derive(Serialize)]
struct JsonWaste {
    kind: &'static str,
    tokens: u64,
    detail: String,
}

#[derive(Serialize)]
struct JsonSavings {
    low: f64,
    high: f64,
}

/// Serialize the scan + waste report to the stable `--json` schema string.
pub fn render(result: &ScanResult, waste: &WasteReport, redact: bool) -> String {
    let categories = result
        .categories
        .iter()
        .map(|c| JsonCategory {
            name: c.category.name(),
            tokens: c.tokens,
            pct: round1(result.category_pct(c.category)),
        })
        .collect();

    let waste_arr = findings(waste, redact)
        .into_iter()
        .map(|f| JsonWaste { kind: f.kind, tokens: f.tokens, detail: f.detail })
        .collect();

    let s = savings(result, waste);

    let report = JsonReport {
        schema: 1,
        window_days: result.window_days,
        totals: JsonTotals {
            tokens: result.totals.tokens,
            cache_read: result.totals.cache_read,
            sessions: result.totals.sessions,
            projects: result.totals.projects,
        },
        categories,
        waste: waste_arr,
        savings_pct: JsonSavings { low: round1(s.low), high: round1(s.high) },
    };

    // Pretty-print: still valid JSON, friendlier for humans piping through `jq`.
    serde_json::to_string_pretty(&report).expect("JsonReport is always serializable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hm_core::{analyze_waste, scan, ScanOptions};
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn build(files: &[&str]) -> ScanOptions {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("json_{n}"));
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("projects").join("p");
        fs::create_dir_all(&proj).unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
        for f in files {
            fs::copy(fx.join(f), proj.join(f)).unwrap();
        }
        ScanOptions { claude_dir: Some(root), days: 7, project_filter: None }
    }

    #[test]
    fn json_round_trips_and_matches_schema() {
        let o = build(&["normal-session.jsonl", "repeated-reads.jsonl"]);
        let r = scan(&o).unwrap();
        let w = analyze_waste(&o).unwrap();
        let text = render(&r, &w, false);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(v["schema"], 1);
        assert_eq!(v["window_days"], 7);
        assert!(v["totals"]["tokens"].is_u64());
        assert!(v["totals"]["cache_read"].is_u64());
        assert!(v["totals"]["sessions"].is_u64());
        assert!(v["totals"]["projects"].is_u64());
        assert!(v["categories"].is_array());
        assert!(v["categories"][0]["name"].is_string());
        assert!(v["categories"][0]["pct"].is_number());
        let waste = v["waste"].as_array().unwrap();
        assert_eq!(waste.len(), 5, "five stable waste kinds");
        let kinds: Vec<_> = waste.iter().map(|w| w["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec!["repeated_reads", "json_blobs", "log_noise", "huge_output", "sub_agents"]
        );
        assert!(v["savings_pct"]["low"].is_number());
        assert!(v["savings_pct"]["high"].is_number());
    }

    #[test]
    fn empty_scan_still_valid_json() {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("json_empty_{n}"));
        fs::create_dir_all(root.join("projects")).unwrap();
        let o = ScanOptions { claude_dir: Some(root), days: 7, project_filter: None };
        let r = scan(&o).unwrap();
        let w = analyze_waste(&o).unwrap();
        let v: serde_json::Value = serde_json::from_str(&render(&r, &w, false)).unwrap();
        assert_eq!(v["totals"]["sessions"], 0);
        assert_eq!(v["waste"].as_array().unwrap().len(), 5);
    }
}
