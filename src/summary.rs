//! Derived, render-agnostic summary of a scan + waste report (spec 01 §4).
//!
//! The terminal, JSON, and HTML renderers all build from this so their numbers
//! and wording stay in lockstep. Nothing here does I/O.

use hm_core::{ScanResult, SubAgentShare, WasteReport};

use crate::render::{display_path, format_tokens, round1};

/// One "Waste found" finding, already privacy-processed and human-worded.
pub struct Finding {
    /// Stable machine key for the JSON `waste[].kind` field.
    pub kind: &'static str,
    /// Token count attributed to this finding (JSON `waste[].tokens`).
    pub tokens: u64,
    /// Human sentence for the terminal / HTML and JSON `waste[].detail`.
    pub detail: String,
}

/// The five stable waste kinds in spec order (spec 01 §4). Every scan emits all
/// five (zero tokens when a detector found nothing) so the JSON schema is stable.
pub fn findings(waste: &WasteReport, redact: bool) -> Vec<Finding> {
    let rr = &waste.repeated_reads;
    let reread_detail = if rr.file_count == 0 {
        "no repeated file reads".to_string()
    } else {
        let top = rr
            .offenders
            .first()
            .map(|o| format!("  (top: {} ×{})", display_path(&o.path, redact), o.reads))
            .unwrap_or_default();
        format!(
            "{} tokens re-reading the same {} file{}{}",
            format_tokens(rr.wasted_tokens),
            rr.file_count,
            if rr.file_count == 1 { "" } else { "s" },
            top,
        )
    };

    let (huge_tokens, huge_detail) = match waste.huge_outputs.first() {
        Some(top) => {
            let total: u64 = waste.huge_outputs.iter().map(|h| h.tokens).sum();
            (
                total,
                format!(
                    "Top offender: {} output, {} tokens (project {})",
                    top.tool,
                    format_tokens(top.tokens),
                    top.project,
                ),
            )
        }
        None => (0, "no single output over 20 KB".to_string()),
    };

    let (sub_tokens, sub_detail) = match waste.sub_agent {
        SubAgentShare::Known { sidechain_tokens, pct, .. } => (
            sidechain_tokens,
            format!("Sub-agents: {:.1}% of total burn", round1(pct)),
        ),
        SubAgentShare::Unknown => (0, "Sub-agents: no sidechain markers in window".to_string()),
    };

    // Exactly the five spec kinds, in spec order. The terminal/HTML "compressible
    // JSON/log" bullet combines json_blobs + log_noise, but JSON keeps them
    // separate here so each detector round-trips independently.
    vec![
        Finding { kind: "repeated_reads", tokens: rr.wasted_tokens, detail: reread_detail },
        Finding { kind: "json_blobs", tokens: waste.json_blobs.wasted_tokens, detail: json_detail(waste) },
        Finding { kind: "log_noise", tokens: waste.log_noise.wasted_tokens, detail: log_detail(waste) },
        Finding { kind: "huge_output", tokens: huge_tokens, detail: huge_detail },
        Finding { kind: "sub_agents", tokens: sub_tokens, detail: sub_detail },
    ]
}

fn json_detail(waste: &WasteReport) -> String {
    let b = &waste.json_blobs;
    if b.count == 0 {
        "no giant JSON blobs".to_string()
    } else {
        format!(
            "{} tokens across {} giant JSON blob{} (>20 items or >8 KB)",
            format_tokens(b.wasted_tokens),
            b.count,
            if b.count == 1 { "" } else { "s" },
        )
    }
}

fn log_detail(waste: &WasteReport) -> String {
    let l = &waste.log_noise;
    if l.count == 0 {
        "no repetitive log output".to_string()
    } else {
        format!(
            "{} tokens across {} log-noise output{} (>50 lines, >30% duplicate)",
            format_tokens(l.wasted_tokens),
            l.count,
            if l.count == 1 { "" } else { "s" },
        )
    }
}

/// The three headline "Waste found" bullet strings for the terminal / HTML,
/// already redaction-processed (spec 01 §4 layout).
pub fn waste_bullets(waste: &WasteReport, redact: bool) -> Vec<String> {
    let mut out = Vec::new();
    let rr = &waste.repeated_reads;
    if rr.file_count > 0 {
        let top = rr
            .offenders
            .first()
            .map(|o| format!("  (top: {} ×{})", display_path(&o.path, redact), o.reads))
            .unwrap_or_default();
        out.push(format!(
            "{} tokens re-reading the same {} file{}{}",
            format_tokens(rr.wasted_tokens),
            rr.file_count,
            if rr.file_count == 1 { "" } else { "s" },
            top,
        ));
    }

    let compressible = waste.json_blobs.wasted_tokens + waste.log_noise.wasted_tokens;
    if compressible > 0 {
        out.push(format!(
            "{} tokens in compressible JSON/log output",
            format_tokens(compressible),
        ));
    }

    if let Some(top) = waste.huge_outputs.first() {
        out.push(format!(
            "Top offender: {} output, {} tokens (project {})",
            top.tool,
            format_tokens(top.tokens),
            top.project,
        ));
    }

    if let SubAgentShare::Known { pct, .. } = waste.sub_agent {
        if pct > 0.0 {
            out.push(format!("Sub-agents drove {:.1}% of total burn", round1(pct)));
        }
    }

    if out.is_empty() {
        out.push("No significant waste detected in this window.".to_string());
    }
    out
}

/// Savings range as rounded whole-percent bounds plus the "extra days" estimate
/// (spec 01 §4). Extra days = midpoint savings_pct × window_days / 100, 1 dp.
pub struct Savings {
    pub low: f64,
    pub high: f64,
    pub extra_days: f64,
}

pub fn savings(result: &ScanResult, waste: &WasteReport) -> Savings {
    let low = waste.savings.low;
    let high = waste.savings.high;
    let midpoint = (low + high) / 2.0;
    let extra_days = round1(midpoint * result.window_days as f64 / 100.0);
    Savings { low, high, extra_days }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hm_core::{analyze_waste, scan, ScanOptions};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
    }

    fn claude_with(files: &[&str]) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("sum_{n}"));
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("projects").join("p");
        fs::create_dir_all(&proj).unwrap();
        for f in files {
            fs::copy(fixtures().join(f), proj.join(f)).unwrap();
        }
        root
    }

    fn opts(dir: PathBuf) -> ScanOptions {
        ScanOptions { claude_dir: Some(dir), days: 7, project_filter: None }
    }

    #[test]
    fn findings_always_has_five_kinds() {
        let o = opts(claude_with(&["repeated-reads.jsonl"]));
        let w = analyze_waste(&o).unwrap();
        let f = findings(&w, false);
        let kinds: Vec<_> = f.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            vec!["repeated_reads", "json_blobs", "log_noise", "huge_output", "sub_agents"]
        );
    }

    #[test]
    fn repeated_read_bullet_redacts_path() {
        let o = opts(claude_with(&["repeated-reads.jsonl"]));
        let w = analyze_waste(&o).unwrap();
        let plain = waste_bullets(&w, false);
        let redacted = waste_bullets(&w, true);
        assert!(plain.iter().any(|b| b.contains("big_module")));
        assert!(!redacted.iter().any(|b| b.contains("big_module")));
        assert!(redacted.iter().any(|b| b.contains("path#")));
    }

    #[test]
    fn savings_extra_days_uses_midpoint() {
        let o = opts(claude_with(&["json-blob.jsonl"]));
        let r = scan(&o).unwrap();
        let w = analyze_waste(&o).unwrap();
        let s = savings(&r, &w);
        let mid = (s.low + s.high) / 2.0;
        let expect = ((mid * 7.0 / 100.0) * 10.0).round() / 10.0;
        assert!((s.extra_days - expect).abs() < 1e-9);
    }
}
