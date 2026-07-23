//! Integration tests: spawn the built `hypermile-audit` binary and assert on
//! exit codes and stdout/stderr against the shared fixtures (spec 01 §6).
//!
//! Each test builds a throwaway `<tmp>/projects/<proj>/*.jsonl` tree (fixture
//! mtimes become "now", inside the default window) and points the binary at it
//! with `--claude-dir`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use assert_cmd::Command;
use predicates::prelude::*;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

/// Build a claude dir under Cargo's per-test tmp dir; returns its path.
fn build_claude_dir(layout: &[(&str, &[&str])]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli_{n}"));
    let _ = fs::remove_dir_all(&root);
    let src = fixtures_dir();
    for (project, files) in layout {
        let proj_dir = root.join("projects").join(project);
        fs::create_dir_all(&proj_dir).unwrap();
        for file in *files {
            fs::copy(src.join(file), proj_dir.join(file)).unwrap();
        }
    }
    root
}

fn bin() -> Command {
    Command::cargo_bin("hypermile-audit").unwrap()
}

#[test]
fn default_run_prints_report_and_exits_zero() {
    let dir = build_claude_dir(&[("proj-main", &["normal-session.jsonl", "repeated-reads.jsonl"])]);
    bin()
        .arg("--claude-dir")
        .arg(&dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("AUDIT — last 7 days"))
        .stdout(predicate::str::contains("Where it went:"))
        .stdout(predicate::str::contains("Waste found:"))
        .stdout(predicate::str::contains("Estimated savings"))
        .stdout(predicate::str::contains("/download?src=audit"));
}

#[test]
fn no_color_output_is_plain() {
    let dir = build_claude_dir(&[("p", &["normal-session.jsonl"])]);
    let out = bin()
        .arg("--claude-dir")
        .arg(&dir)
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains('\u{1b}'), "NO_COLOR output must contain no ANSI escapes");
}

#[test]
fn piped_output_is_plain_without_no_color() {
    // assert_cmd captures stdout via a pipe (not a TTY), so even without
    // NO_COLOR the output must be plain (spec 01 §4).
    let dir = build_claude_dir(&[("p", &["normal-session.jsonl"])]);
    let out = bin().arg("--claude-dir").arg(&dir).assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains('\u{1b}'), "piped (non-TTY) output must be plain");
}

#[test]
fn json_flag_emits_only_valid_schema() {
    let dir = build_claude_dir(&[("proj-main", &["normal-session.jsonl", "repeated-reads.jsonl"])]);
    let out = bin()
        .arg("--claude-dir")
        .arg(&dir)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    // Parse and validate the schema (spec 01 §4 / §6.4).
    let v: serde_json::Value = serde_json::from_str(&text).expect("stdout must be valid JSON");
    assert_eq!(v["schema"], 1);
    assert_eq!(v["window_days"], 7);
    assert!(v["totals"]["tokens"].is_u64());
    assert!(v["totals"]["cache_read"].is_u64());
    assert!(v["totals"]["sessions"].is_u64());
    assert!(v["totals"]["projects"].is_u64());
    assert!(v["categories"].is_array());
    let waste = v["waste"].as_array().unwrap();
    assert_eq!(waste.len(), 5);
    for w in waste {
        assert!(w["kind"].is_string());
        assert!(w["tokens"].is_u64());
        assert!(w["detail"].is_string());
    }
    assert!(v["savings_pct"]["low"].is_number());
    assert!(v["savings_pct"]["high"].is_number());
    // Nothing but JSON on stdout: it must parse whole (no report text mixed in).
    assert!(text.trim_start().starts_with('{'));
}

#[test]
fn missing_dir_is_friendly_and_exits_zero() {
    let dir = PathBuf::from("this/does/not/exist/anywhere");
    bin()
        .arg("--claude-dir")
        .arg(&dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("no Claude Code transcripts found"));
}

#[test]
fn redact_paths_hides_real_paths() {
    let dir = build_claude_dir(&[("proj-reads", &["repeated-reads.jsonl"])]);
    // Without redaction the real path appears...
    bin()
        .arg("--claude-dir")
        .arg(&dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("big_module"));
    // ...with --redact-paths it must not, and a hash token appears instead.
    bin()
        .arg("--claude-dir")
        .arg(&dir)
        .arg("--redact-paths")
        .assert()
        .success()
        .stdout(predicate::str::contains("big_module").not())
        .stdout(predicate::str::contains("path#"));
}

#[test]
fn html_flag_writes_self_contained_file() {
    let dir = build_claude_dir(&[("p", &["normal-session.jsonl", "json-blob.jsonl"])]);
    let out_path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("report.html");
    let _ = fs::remove_file(&out_path);
    bin()
        .arg("--claude-dir")
        .arg(&dir)
        .arg("--html")
        .arg(&out_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Wrote HTML report"));
    let html = fs::read_to_string(&out_path).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<style>"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("<link"));
    assert!(html.contains("/download?src=audit"));
}

#[test]
fn help_and_version_exit_zero() {
    bin().arg("--help").assert().success().stdout(predicate::str::contains("USAGE:"));
    bin()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("hypermile-audit"));
}

#[test]
fn unknown_flag_exits_two() {
    bin()
        .arg("--nonsense")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown argument"));
}

#[test]
fn project_filter_limits_scope() {
    let dir = build_claude_dir(&[
        ("alpha-web", &["normal-session.jsonl"]),
        ("beta-api", &["string-content.jsonl"]),
    ]);
    let out = bin()
        .arg("--claude-dir")
        .arg(&dir)
        .arg("--project")
        .arg("alpha")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8(out).unwrap()).unwrap();
    assert_eq!(v["totals"]["projects"], 1);
    assert_eq!(v["totals"]["sessions"], 1);
}

#[test]
fn days_flag_parsed() {
    let dir = build_claude_dir(&[("p", &["normal-session.jsonl"])]);
    let out = bin()
        .arg("--claude-dir")
        .arg(&dir)
        .arg("--days")
        .arg("30")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_str(&String::from_utf8(out).unwrap()).unwrap();
    assert_eq!(v["window_days"], 30);
}
