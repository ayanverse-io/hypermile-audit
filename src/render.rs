//! Shared, output-agnostic formatting helpers (spec 01 §4–§5).
//!
//! These functions are pure and format-neutral so the terminal, JSON, and HTML
//! renderers all agree on numbers, bar widths, and privacy handling.

/// Number of cells in a category bar. 10 cells == 100 % (absolute scale).
pub const BAR_CELLS: usize = 10;

/// Human-format a token count like the spec's `69.9M` / `94.1M` (spec 01 §4).
///
/// Thousands → `K`, millions → `M`, billions → `B`, each with one decimal;
/// under 1000 renders as the plain integer.
pub fn format_tokens(n: u64) -> String {
    const K: f64 = 1_000.0;
    const M: f64 = 1_000_000.0;
    const B: f64 = 1_000_000_000.0;
    let f = n as f64;
    if f >= B {
        format!("{:.1}B", f / B)
    } else if f >= M {
        format!("{:.1}M", f / M)
    } else if f >= K {
        format!("{:.1}K", f / K)
    } else {
        n.to_string()
    }
}

/// A unicode block-char bar for `pct` (0–100) on an absolute 10-cell scale.
///
/// We use an absolute scale (10 cells == 100 %) rather than the illustrative
/// widths sketched in spec §4 so the bar is self-consistent with its own
/// percentage label — a 50 % category always fills exactly half the bar.
pub fn bar(pct: f64) -> String {
    let filled = ((pct / 100.0) * BAR_CELLS as f64).round() as usize;
    let filled = filled.min(BAR_CELLS);
    let mut s = String::with_capacity(BAR_CELLS * 3);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in 0..(BAR_CELLS - filled) {
        s.push('░');
    }
    s
}

/// Round a percentage to one decimal place (for JSON / HTML stability).
pub fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Apply the privacy rule for a file path (spec 01 §5): by default the path is
/// shown verbatim (it is a *name*, never file content); with `--redact-paths`
/// it is replaced by a stable 8-char hash so reports can be shared publicly.
pub fn display_path(path: &str, redact: bool) -> String {
    if redact {
        format!("path#{}", hash8(path))
    } else {
        path.to_string()
    }
}

/// Truncate a command/string to 60 chars with an ellipsis (spec 01 §5). We never
/// surface raw commands in any current output (privacy by omission), so this is
/// not wired to a renderer yet; it exists so any future command display stays
/// within the spec's 60-char bound. Kept + unit-tested to lock the behavior.
#[allow(dead_code)]
pub fn truncate60(s: &str) -> String {
    const MAX: usize = 60;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX - 1).collect();
    format!("{head}…")
}

/// Stable 8-hex-char redaction token via FNV-1a (dependency-free, deterministic
/// across runs so a redacted path is consistent within a report).
fn hash8(s: &str) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001B3;
    let mut h = OFFSET;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    format!("{:08x}", (h & 0xffff_ffff) as u32)
}

// ---------------------------------------------------------------------------
// Color palette (spec 01 §4): honor NO_COLOR and non-TTY (plain when piped).
// ---------------------------------------------------------------------------

use anstyle::{AnsiColor, Style};

/// Whether ANSI styling should be emitted. False when `NO_COLOR` is set (any
/// value) or when stdout is not a terminal (piped / redirected / `--json`).
pub fn colors_enabled(stdout_is_tty: bool) -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    stdout_is_tty
}

/// Wrap `text` in `style` when `enabled`, otherwise return it unstyled.
pub fn paint(enabled: bool, style: Style, text: &str) -> String {
    if enabled {
        format!("{}{}{}", style.render(), text, style.render_reset())
    } else {
        text.to_string()
    }
}

/// Bold style (headers, section titles).
pub fn bold() -> Style {
    Style::new().bold()
}

/// Bold green (the savings headline).
pub fn good() -> Style {
    Style::new().bold().fg_color(Some(AnsiColor::Green.into()))
}

/// Cyan (the download link).
pub fn link() -> Style {
    Style::new().fg_color(Some(AnsiColor::Cyan.into()))
}

/// Dimmed (secondary detail such as cache reads).
pub fn dim() -> Style {
    Style::new().dimmed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_millions_with_one_decimal() {
        assert_eq!(format_tokens(182_400_000), "182.4M");
        assert_eq!(format_tokens(94_100_000), "94.1M");
        assert_eq!(format_tokens(69_900_000), "69.9M");
    }

    #[test]
    fn formats_thousands_and_billions_and_small() {
        assert_eq!(format_tokens(4_200), "4.2K");
        assert_eq!(format_tokens(2_300_000_000), "2.3B");
        assert_eq!(format_tokens(512), "512");
        assert_eq!(format_tokens(0), "0");
    }

    #[test]
    fn bar_is_ten_cells_and_scales() {
        assert_eq!(bar(0.0).chars().count(), BAR_CELLS);
        assert_eq!(bar(100.0), "██████████");
        assert_eq!(bar(50.0), "█████░░░░░");
        assert_eq!(bar(0.0), "░░░░░░░░░░");
        // Over-100 (shouldn't happen) clamps rather than overflowing.
        assert_eq!(bar(250.0), "██████████");
    }

    #[test]
    fn redaction_is_stable_and_hides_the_path() {
        let a = display_path("src/config/big_module.ts", true);
        let b = display_path("src/config/big_module.ts", true);
        assert_eq!(a, b, "same path redacts identically");
        assert!(!a.contains("big_module"), "original name must not leak");
        assert!(a.starts_with("path#") && a.len() == "path#".len() + 8);
        // Distinct paths → distinct tokens (overwhelmingly likely).
        assert_ne!(a, display_path("src/other.ts", true));
    }

    #[test]
    fn no_redaction_shows_verbatim() {
        assert_eq!(display_path("src/main.rs", false), "src/main.rs");
    }

    #[test]
    fn truncate60_bounds_length() {
        let long = "x".repeat(200);
        let t = truncate60(&long);
        assert_eq!(t.chars().count(), 60);
        assert!(t.ends_with('…'));
        assert_eq!(truncate60("short"), "short");
    }

    #[test]
    fn no_color_env_disables_colors() {
        // Can't safely mutate process env in parallel tests; assert the TTY gate.
        assert!(!colors_enabled(false), "non-tty is always plain");
    }
}
