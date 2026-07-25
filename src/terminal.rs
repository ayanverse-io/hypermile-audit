//! The colored terminal report (spec 01 §4 layout).
//!
//! `render` returns the whole report as a `String` (no direct printing) so it is
//! trivially testable and so `main` controls where it is written.

use hm_core::{ScanResult, WasteReport, BASE_URL, BRAND};

use crate::render::{bar, bold, colors_enabled, dim, format_tokens, good, link, paint};
use crate::summary::{savings, waste_bullets};

/// Width the leader dots pad the totals label to.
const LEADER_WIDTH: usize = 30;
/// Width category names are padded to before the token figure.
const NAME_WIDTH: usize = 26;

/// Render the full terminal report. `stdout_is_tty` gates ANSI styling (together
/// with `NO_COLOR`); pass `false` to force plain output.
pub fn render(result: &ScanResult, waste: &WasteReport, redact: bool, stdout_is_tty: bool) -> String {
    let color = colors_enabled(stdout_is_tty);
    let mut out = String::new();

    // Header line.
    let header = format!(
        "{} AUDIT — last {} days, {} project{}, {} session{}",
        BRAND.to_uppercase(),
        result.window_days,
        result.totals.projects,
        plural(result.totals.projects),
        result.totals.sessions,
        plural(result.totals.sessions),
    );
    out.push_str(&paint(color, bold(), &header));
    out.push('\n');

    // Totals line with dotted leader.
    let label = "Total context processed";
    let dots = ".".repeat(LEADER_WIDTH.saturating_sub(label.len()).max(1));
    let cache = paint(
        color,
        dim(),
        &format!("({} cache reads)", format_tokens(result.totals.cache_read)),
    );
    out.push_str(&format!(
        "{label} {dots} {} tokens  {cache}\n",
        format_tokens(result.totals.tokens),
    ));

    if result.parse_errors > 0 {
        out.push_str(&paint(
            color,
            dim(),
            &format!("({} unparseable line(s) skipped)\n", result.parse_errors),
        ));
    }

    // Where it went.
    out.push('\n');
    out.push_str(&paint(color, bold(), "Where it went:"));
    out.push('\n');
    for cat in &result.categories {
        let pct = result.category_pct(cat.category);
        let name = cat.category.name();
        out.push_str(&format!(
            "  {} {:>3.0}%  {:<width$} {}\n",
            bar(pct),
            pct,
            name,
            format_tokens(cat.tokens),
            width = NAME_WIDTH,
        ));
    }

    // Waste found.
    out.push('\n');
    out.push_str(&paint(color, bold(), "Waste found:"));
    out.push('\n');
    for bullet in waste_bullets(waste, redact) {
        out.push_str(&format!("  • {bullet}\n"));
    }

    // Savings + download link.
    let s = savings(result, waste);
    out.push('\n');
    let headline = format!(
        "Estimated savings with {BRAND}: {:.0}–{:.0}%  (≈ {:.1} extra days before your weekly cap)",
        s.low, s.high, s.extra_days,
    );
    out.push_str(&paint(color, good(), &headline));
    out.push('\n');
    let url = format!("{BASE_URL}/download?src=audit");
    out.push_str(&format!("→ Get Hypermile: {}\n", paint(color, link(), &url)));

    out
}

/// The friendly, exit-0 message shown when no transcripts were found (spec §6).
pub fn empty_message(window_days: u32) -> String {
    format!(
        "{BRAND} audit: no Claude Code transcripts found in the last {window_days} days.\n\
         Nothing to analyze yet — come back after some coding sessions.\n\
         Learn more: {BASE_URL}/download?src=audit\n"
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
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
        let root = std::env::temp_dir().join(format!("term_{n}"));
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
    fn plain_output_has_no_ansi_and_all_sections() {
        let o = build(&["normal-session.jsonl", "repeated-reads.jsonl", "json-blob.jsonl"]);
        let r = scan(&o).unwrap();
        let w = analyze_waste(&o).unwrap();
        let text = render(&r, &w, false, false);
        assert!(!text.contains('\u{1b}'), "no ANSI escapes when tty=false");
        assert!(text.contains("AUDIT — last 7 days"));
        assert!(text.contains("Total context processed"));
        assert!(text.contains("Where it went:"));
        assert!(text.contains("Waste found:"));
        assert!(text.contains("Estimated savings"));
        assert!(text.contains("/download?src=audit"));
    }

    #[test]
    fn redaction_hides_paths_in_terminal() {
        let o = build(&["repeated-reads.jsonl"]);
        let r = scan(&o).unwrap();
        let w = analyze_waste(&o).unwrap();
        let plain = render(&r, &w, false, false);
        let red = render(&r, &w, true, false);
        assert!(plain.contains("big_module"));
        assert!(!red.contains("big_module"));
    }
}
