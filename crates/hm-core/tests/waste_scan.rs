//! End-to-end integration tests for `hm_core::analyze_waste`, driving the public
//! API through the injectable `claude_dir` against the shared fixtures in
//! `tests/fixtures/`.
//!
//! Mirrors the harness in `fixtures_scan.rs`: each test builds a throwaway
//! `<tmp>/projects/<proj>/...` tree, copies fixtures in (so their mtime is
//! inside the window), then analyzes it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use hm_core::{analyze_waste, BlobKind, ScanOptions, SubAgentShare};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn build_claude_dir(layout: &[(&str, &[&str])]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("waste_{n}"));
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

fn opts(dir: PathBuf) -> ScanOptions {
    ScanOptions {
        claude_dir: Some(dir),
        days: 7,
        project_filter: None,
    }
}

#[test]
fn repeated_reads_fixture_flags_the_repeated_file() {
    let dir = build_claude_dir(&[("proj-reads", &["repeated-reads.jsonl"])]);
    let r = analyze_waste(&opts(dir)).unwrap();
    // big_module.ts is read three times → one offender, three reads, waste > 0.
    assert_eq!(r.repeated_reads.file_count, 1);
    assert_eq!(r.repeated_reads.offenders.len(), 1);
    let off = &r.repeated_reads.offenders[0];
    assert_eq!(off.path, "src/config/big_module.ts");
    assert_eq!(off.reads, 3);
    assert!(off.wasted_tokens > 0);
    assert_eq!(r.repeated_reads.wasted_tokens, off.wasted_tokens);
}

#[test]
fn json_blob_fixture_flags_array_and_oversize() {
    let dir = build_claude_dir(&[("proj-json", &["json-blob.jsonl"])]);
    let r = analyze_waste(&opts(dir)).unwrap();
    // Two blobs flagged (large array + >8KB object); the small [1,2,3] is not.
    assert_eq!(r.json_blobs.count, 2);
    assert!(r.json_blobs.wasted_tokens > 0);
    let kinds: Vec<BlobKind> = r.json_blobs.samples.iter().map(|s| s.kind).collect();
    assert!(kinds.contains(&BlobKind::LargeArray));
    assert!(kinds.contains(&BlobKind::Oversize));
    // The >8KB object result is also large enough to be a huge output? 9KB < 20KB,
    // so it must NOT appear as a huge single output.
    assert!(r.huge_outputs.is_empty(), "9KB blob is under the 20KB huge threshold");
}

#[test]
fn log_noise_fixture_is_flagged_as_compressible() {
    let dir = build_claude_dir(&[("proj-log", &["log-noise.jsonl"])]);
    let r = analyze_waste(&opts(dir)).unwrap();
    assert_eq!(r.log_noise.count, 1);
    assert!(r.log_noise.wasted_tokens > 0);
    let sample = &r.log_noise.samples[0];
    assert_eq!(sample.lines, 120);
    assert!(sample.duplicate_pct > 30, "duplicate_pct was {}", sample.duplicate_pct);
    assert_eq!(sample.tool, "Bash");
}

#[test]
fn huge_output_fixture_lists_top_offender() {
    let dir = build_claude_dir(&[("proj-huge", &["huge-tool-result.jsonl"])]);
    let r = analyze_waste(&opts(dir)).unwrap();
    // The 26KB fixture tool_result exceeds the 20KB threshold.
    assert!(!r.huge_outputs.is_empty());
    assert!(r.huge_outputs[0].bytes > 20 * 1024);
    assert_eq!(r.huge_outputs[0].project, "proj-huge");
}

#[test]
fn sidechain_fixture_reports_known_share() {
    let dir = build_claude_dir(&[("proj-sc", &["sidechain.jsonl"])]);
    let r = analyze_waste(&opts(dir)).unwrap();
    match r.sub_agent {
        SubAgentShare::Known { sidechain_tokens, total_tokens, pct } => {
            assert!(sidechain_tokens > 0);
            assert!(total_tokens > sidechain_tokens);
            assert!(pct > 0.0 && pct < 100.0);
        }
        SubAgentShare::Unknown => panic!("sidechain marker present → expected Known"),
    }
}

#[test]
fn transcripts_without_marker_report_unknown_share() {
    let dir = build_claude_dir(&[("proj-normal", &["normal-session.jsonl"])]);
    let r = analyze_waste(&opts(dir)).unwrap();
    assert_eq!(r.sub_agent, SubAgentShare::Unknown);
}

#[test]
fn savings_range_is_within_zero_to_65_and_ordered() {
    let dir = build_claude_dir(&[(
        "proj-all",
        &["json-blob.jsonl", "log-noise.jsonl", "repeated-reads.jsonl"],
    )]);
    let r = analyze_waste(&opts(dir)).unwrap();
    assert!(r.savings.low >= 0.0 && r.savings.high <= 65.0);
    assert!(r.savings.low <= r.savings.high);
    assert!(r.savings.high > 0.0, "fixtures contain real waste");
}

#[test]
fn empty_projects_dir_yields_zero_waste() {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("waste_empty_{n}"));
    fs::create_dir_all(root.join("projects")).unwrap();
    let r = analyze_waste(&opts(root)).unwrap();
    assert_eq!(r.repeated_reads.wasted_tokens, 0);
    assert_eq!(r.json_blobs.count, 0);
    assert_eq!(r.log_noise.count, 0);
    assert!(r.huge_outputs.is_empty());
    assert_eq!(r.sub_agent, SubAgentShare::Unknown);
    assert_eq!(r.savings.low, 0.0);
    assert_eq!(r.savings.high, 0.0);
}

#[test]
fn malformed_lines_do_not_break_waste_analysis() {
    let dir = build_claude_dir(&[("proj-bad", &["malformed-lines.jsonl"])]);
    // Must not panic; malformed lines are skipped like in `scan`.
    let r = analyze_waste(&opts(dir)).unwrap();
    assert_eq!(r.repeated_reads.wasted_tokens, 0);
}
