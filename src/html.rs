//! The `--html` self-contained shareable report (spec 01 §4).
//!
//! A single file: inline CSS only, no external assets, no JavaScript, pure-CSS
//! bar charts, dark-friendly. Renders standalone in any browser. Carries the
//! same data as the terminal report plus the download link.

use hm_core::{ScanResult, WasteReport, BASE_URL, BRAND};

use crate::render::{format_tokens, round1};
use crate::summary::{findings, savings, waste_bullets};

/// Minimal HTML-escaping for text interpolated into the page (paths, tool and
/// project names, detail sentences). Prevents a `<` in a path from breaking the
/// document; never emits raw user content beyond names (spec §5).
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the complete self-contained HTML document.
pub fn render(result: &ScanResult, waste: &WasteReport, redact: bool) -> String {
    let s = savings(result, waste);
    let header = format!(
        "{} Audit — last {} days, {} projects, {} sessions",
        BRAND, result.window_days, result.totals.projects, result.totals.sessions
    );

    let mut categories = String::new();
    for cat in &result.categories {
        let pct = round1(result.category_pct(cat.category));
        categories.push_str(&format!(
            r#"      <div class="row">
        <div class="label">{name}</div>
        <div class="track"><div class="fill" style="width:{pct}%"></div></div>
        <div class="pct">{pct:.1}%</div>
        <div class="tok">{tokens}</div>
      </div>
"#,
            name = esc(cat.category.name()),
            pct = pct,
            tokens = esc(&format_tokens(cat.tokens)),
        ));
    }

    let mut bullets = String::new();
    for b in waste_bullets(waste, redact) {
        bullets.push_str(&format!("      <li>{}</li>\n", esc(&b)));
    }

    // A detail table over the five stable finding kinds (mirrors --json).
    let mut findings_rows = String::new();
    for f in findings(waste, redact) {
        findings_rows.push_str(&format!(
            "      <tr><td class=\"k\">{}</td><td class=\"n\">{}</td><td>{}</td></tr>\n",
            esc(f.kind),
            esc(&format_tokens(f.tokens)),
            esc(&f.detail),
        ));
    }

    let url = format!("{BASE_URL}/download?src=audit");

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  :root {{
    --bg: #0e1116; --panel: #161b22; --fg: #e6edf3; --muted: #8b949e;
    --accent: #3fb950; --bar: #388bfd; --track: #21262d; --border: #30363d;
  }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; padding: 2rem 1rem; background: var(--bg); color: var(--fg);
    font: 15px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  }}
  .wrap {{ max-width: 760px; margin: 0 auto; }}
  h1 {{ font-size: 1.35rem; margin: 0 0 .25rem; }}
  .sub {{ color: var(--muted); margin: 0 0 1.5rem; }}
  .card {{
    background: var(--panel); border: 1px solid var(--border);
    border-radius: 10px; padding: 1.1rem 1.25rem; margin: 0 0 1.25rem;
  }}
  .card h2 {{ font-size: .8rem; text-transform: uppercase; letter-spacing: .06em;
    color: var(--muted); margin: 0 0 .9rem; }}
  .totals {{ font-size: 1.05rem; }}
  .totals strong {{ font-size: 1.5rem; }}
  .totals .cache {{ color: var(--muted); }}
  .row {{ display: grid; grid-template-columns: 12rem 1fr 3.5rem 4.5rem;
    align-items: center; gap: .6rem; margin: .35rem 0; }}
  .label {{ color: var(--fg); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
  .track {{ background: var(--track); border-radius: 5px; height: 12px; overflow: hidden; }}
  .fill {{ background: var(--bar); height: 100%; border-radius: 5px; }}
  .pct {{ text-align: right; color: var(--muted); font-variant-numeric: tabular-nums; }}
  .tok {{ text-align: right; font-variant-numeric: tabular-nums; }}
  ul {{ margin: 0; padding-left: 1.2rem; }}
  ul li {{ margin: .3rem 0; }}
  table {{ width: 100%; border-collapse: collapse; margin-top: .3rem; }}
  td {{ padding: .35rem .4rem; border-top: 1px solid var(--border); vertical-align: top; }}
  td.k {{ color: var(--bar); white-space: nowrap; font-family: ui-monospace, monospace; }}
  td.n {{ text-align: right; white-space: nowrap; font-variant-numeric: tabular-nums; }}
  .savings {{ font-size: 1.25rem; color: var(--accent); font-weight: 700; }}
  .cta a {{ color: var(--bar); text-decoration: none; }}
  .cta a:hover {{ text-decoration: underline; }}
  footer {{ color: var(--muted); font-size: .8rem; margin-top: 1.5rem; text-align: center; }}
  @media (prefers-color-scheme: light) {{
    :root {{ --bg: #ffffff; --panel: #f6f8fa; --fg: #1f2328; --muted: #656d76;
      --track: #eaeef2; --border: #d0d7de; }}
  }}
</style>
</head>
<body>
  <div class="wrap">
    <h1>{brand} Audit</h1>
    <p class="sub">{header}</p>

    <div class="card totals">
      <h2>Total context processed</h2>
      <strong>{total} tokens</strong>
      <span class="cache">&nbsp;({cache} cache reads)</span>
    </div>

    <div class="card">
      <h2>Where it went</h2>
{categories}    </div>

    <div class="card">
      <h2>Waste found</h2>
      <ul>
{bullets}      </ul>
      <table>
{findings}      </table>
    </div>

    <div class="card cta">
      <p class="savings">Estimated savings with {brand}: {low:.0}–{high:.0}%</p>
      <p>≈ {extra_days:.1} extra days before your weekly cap.</p>
      <p>→ Get Hypermile: <a href="{url}">{url}</a></p>
    </div>

    <footer>Generated locally by hypermile-audit · no data left this machine · MIT</footer>
  </div>
</body>
</html>
"#,
        title = esc(&header),
        brand = esc(BRAND),
        header = esc(&header),
        total = esc(&format_tokens(result.totals.tokens)),
        cache = esc(&format_tokens(result.totals.cache_read)),
        categories = categories,
        bullets = bullets,
        findings = findings_rows,
        low = s.low,
        high = s.high,
        extra_days = s.extra_days,
        url = esc(&url),
    )
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
        let root = std::env::temp_dir().join(format!("html_{n}"));
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
    fn html_is_self_contained_and_has_download_link() {
        let o = build(&["normal-session.jsonl", "json-blob.jsonl"]);
        let r = scan(&o).unwrap();
        let w = analyze_waste(&o).unwrap();
        let html = render(&r, &w, false);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<style>"), "inline CSS present");
        assert!(!html.contains("<script"), "no JavaScript");
        assert!(!html.contains("http-equiv"));
        // No external asset references (only the download anchor uses http).
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("<link"));
        assert!(html.contains("/download?src=audit"));
        assert!(html.contains("Where it went"));
        assert!(html.contains("Waste found"));
    }

    #[test]
    fn html_escapes_and_redacts() {
        let o = build(&["repeated-reads.jsonl"]);
        let r = scan(&o).unwrap();
        let w = analyze_waste(&o).unwrap();
        let red = render(&r, &w, true);
        assert!(!red.contains("big_module"), "redacted path must not appear");
    }
}
