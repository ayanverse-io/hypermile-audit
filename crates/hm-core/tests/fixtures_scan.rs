//! End-to-end integration tests for `hm_core::scan`, driving the public API
//! through the injectable `claude_dir` against the shared fixtures that live in
//! `tests/fixtures/`.
//!
//! Each test builds a throwaway `<tmp>/projects/<proj>/...` tree under Cargo's
//! per-test target tmp dir, copies the requested fixtures in (so their mtime is
//! "now" and inside the window), then scans it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use hm_core::{scan, Category, ScanOptions};

/// Absolute path to the shared fixtures directory.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Build a fresh claude dir; `layout` maps project dir name -> fixture files.
fn build_claude_dir(layout: &[(&str, &[&str])]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("claude_{n}"));
    let _ = fs::remove_dir_all(&root);
    let src = fixtures_dir();

    for (project, files) in layout {
        let proj_dir = root.join("projects").join(project);
        fs::create_dir_all(&proj_dir).unwrap();
        for file in *files {
            fs::copy(src.join(file), proj_dir.join(file))
                .unwrap_or_else(|e| panic!("copy {file}: {e}"));
        }
    }
    root
}

const ALL_FIXTURES: &[&str] = &[
    "normal-session.jsonl",
    "malformed-lines.jsonl",
    "string-content.jsonl",
    "missing-usage.jsonl",
    "huge-tool-result.jsonl",
    "repeated-reads.jsonl",
];

fn cat_tokens(result: &hm_core::ScanResult, category: Category) -> u64 {
    result
        .categories
        .iter()
        .find(|c| c.category == category)
        .map(|c| c.tokens)
        .unwrap_or(0)
}

#[test]
fn scans_all_fixtures_in_one_project() {
    let dir = build_claude_dir(&[("proj-main", ALL_FIXTURES)]);
    let opts = ScanOptions {
        claude_dir: Some(dir),
        days: 7,
        project_filter: None,
    };
    let r = scan(&opts).unwrap();

    // One file == one session.
    assert_eq!(r.totals.sessions, 6, "six fixture files -> six sessions");
    assert_eq!(r.totals.projects, 1);

    // malformed-lines.jsonl contains exactly three unparseable lines.
    assert_eq!(r.parse_errors, 3, "malformed fixture has 3 bad lines");

    // Real usage totals accumulate and include cache reads.
    assert!(r.totals.tokens > 0);
    assert!(r.totals.cache_read > 0);

    // Categories present.
    assert!(cat_tokens(&r, Category::BashOutput) > 0);
    assert!(cat_tokens(&r, Category::FileReads) > 0);
    assert!(cat_tokens(&r, Category::Search) > 0);
    assert!(cat_tokens(&r, Category::ModelOutput) > 0);
    assert!(cat_tokens(&r, Category::YourPrompts) > 0);

    // The 26 KB tool_result dominates Bash output (prose divisor ~ /4).
    assert!(
        cat_tokens(&r, Category::BashOutput) > 5_000,
        "huge tool_result should push Bash output high, got {}",
        cat_tokens(&r, Category::BashOutput)
    );

    // Percentages are well-formed and sum to ~100.
    let sum: f64 = Category::DISPLAY_ORDER
        .iter()
        .map(|&c| r.category_pct(c))
        .sum();
    assert!((sum - 100.0).abs() < 1e-6, "pct sum was {sum}");

    // Project rollup sums to the headline usage total.
    let proj_sum: u64 = r.projects.iter().map(|p| p.tokens).sum();
    assert_eq!(proj_sum, r.totals.tokens);
    assert_eq!(r.projects[0].name, "proj-main");
    assert_eq!(r.projects[0].sessions, 6);
    assert_eq!(r.window_days, 7);
}

#[test]
fn repeated_reads_all_attributed_to_file_reads() {
    let dir = build_claude_dir(&[("proj-reads", &["repeated-reads.jsonl"])]);
    let opts = ScanOptions {
        claude_dir: Some(dir),
        days: 7,
        project_filter: None,
    };
    let r = scan(&opts).unwrap();
    // Three identical Read results all land under File reads.
    assert!(cat_tokens(&r, Category::FileReads) > 0);
    // No Bash output in this fixture.
    assert_eq!(cat_tokens(&r, Category::BashOutput), 0);
    assert_eq!(r.totals.sessions, 1);
}

#[test]
fn string_content_fixture_produces_prompts_and_model_output() {
    let dir = build_claude_dir(&[("proj-str", &["string-content.jsonl"])]);
    let opts = ScanOptions {
        claude_dir: Some(dir),
        days: 7,
        project_filter: None,
    };
    let r = scan(&opts).unwrap();
    assert!(cat_tokens(&r, Category::YourPrompts) > 0, "string user content");
    assert!(cat_tokens(&r, Category::ModelOutput) > 0, "string assistant content");
    assert!(r.totals.tokens > 0, "assistant usage present");
}

#[test]
fn missing_usage_fixture_still_categorizes_but_has_zero_usage() {
    let dir = build_claude_dir(&[("proj-nousage", &["missing-usage.jsonl"])]);
    let opts = ScanOptions {
        claude_dir: Some(dir),
        days: 7,
        project_filter: None,
    };
    let r = scan(&opts).unwrap();
    // No usage blocks -> headline total is zero...
    assert_eq!(r.totals.tokens, 0);
    // ...but estimated categorization still works (Bash tool_result present).
    assert!(cat_tokens(&r, Category::BashOutput) > 0);
    assert!(cat_tokens(&r, Category::YourPrompts) > 0);
}

#[test]
fn project_filter_limits_to_matching_project() {
    let dir = build_claude_dir(&[
        ("alpha-web", &["normal-session.jsonl"]),
        ("beta-api", &["string-content.jsonl"]),
    ]);
    let opts = ScanOptions {
        claude_dir: Some(dir),
        days: 7,
        project_filter: Some("alpha".to_string()),
    };
    let r = scan(&opts).unwrap();
    assert_eq!(r.totals.projects, 1);
    assert_eq!(r.projects[0].name, "alpha-web");
    assert_eq!(r.totals.sessions, 1);
}

#[test]
fn glob_project_filter_matches() {
    let dir = build_claude_dir(&[
        ("alpha-web", &["normal-session.jsonl"]),
        ("beta-api", &["string-content.jsonl"]),
    ]);
    let opts = ScanOptions {
        claude_dir: Some(dir),
        days: 7,
        project_filter: Some("*api".to_string()),
    };
    let r = scan(&opts).unwrap();
    assert_eq!(r.totals.projects, 1);
    assert_eq!(r.projects[0].name, "beta-api");
}

#[test]
fn nested_project_subdirs_are_discovered() {
    // Files can live deeper than the immediate project folder.
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("nested_{n}"));
    let _ = fs::remove_dir_all(&root);
    let nested = root.join("projects").join("deep-proj").join("subdir");
    fs::create_dir_all(&nested).unwrap();
    fs::copy(
        fixtures_dir().join("normal-session.jsonl"),
        nested.join("normal-session.jsonl"),
    )
    .unwrap();

    let opts = ScanOptions {
        claude_dir: Some(root),
        days: 7,
        project_filter: None,
    };
    let r = scan(&opts).unwrap();
    assert_eq!(r.totals.sessions, 1);
    assert_eq!(r.projects[0].name, "deep-proj");
}

#[test]
fn empty_projects_dir_yields_empty_result() {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("empty_{n}"));
    fs::create_dir_all(root.join("projects")).unwrap();
    let opts = ScanOptions {
        claude_dir: Some(root),
        days: 7,
        project_filter: None,
    };
    let r = scan(&opts).unwrap();
    assert!(r.is_empty());
    assert_eq!(r.totals.sessions, 0);
}
