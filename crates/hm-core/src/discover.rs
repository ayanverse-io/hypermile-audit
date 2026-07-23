//! Transcript file discovery (spec 01 §3.1).
//!
//! Finds `<claude-dir>/projects/**/*.jsonl` and filters by file mtime to the
//! requested day window. A missing `projects/` directory is *not* an error —
//! it just yields an empty list (the CLI turns that into a friendly
//! "no transcripts found" message and exits 0).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use walkdir::WalkDir;

/// A discovered transcript file plus the project directory it belongs to.
#[derive(Debug, Clone)]
pub struct TranscriptFile {
    /// Absolute path to the `.jsonl` file.
    pub path: PathBuf,
    /// The immediate `projects/<name>` directory name — our project identifier.
    pub project: String,
}

/// Resolve the effective claude directory: the caller-provided override, or the
/// platform home `.claude` directory when `None`.
pub fn resolve_claude_dir(override_dir: Option<&Path>) -> Option<PathBuf> {
    match override_dir {
        Some(dir) => Some(dir.to_path_buf()),
        None => dirs::home_dir().map(|h| h.join(".claude")),
    }
}

/// Discover transcript files under `<claude_dir>/projects`, keeping only files
/// modified within the last `days` days and (optionally) matching
/// `project_filter` (a substring or simple `*`-glob against the project dir
/// name or full path).
pub fn discover(
    claude_dir: &Path,
    days: u32,
    project_filter: Option<&str>,
) -> Vec<TranscriptFile> {
    let projects_root = claude_dir.join("projects");
    if !projects_root.is_dir() {
        return Vec::new();
    }

    let now = SystemTime::now();
    let mut found = Vec::new();

    for entry in WalkDir::new(&projects_root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if !within_window(path, now, days) {
            continue;
        }

        let project = project_name(&projects_root, path);

        if let Some(filter) = project_filter {
            let path_str = path.to_string_lossy();
            if !matches_filter(&project, &path_str, filter) {
                continue;
            }
        }

        found.push(TranscriptFile {
            path: path.to_path_buf(),
            project,
        });
    }

    found
}

fn within_window(path: &Path, now: SystemTime, days: u32) -> bool {
    match path.metadata().and_then(|m| m.modified()) {
        Ok(mtime) => is_within(mtime, now, days),
        // If we cannot read the mtime, keep the file rather than silently
        // dropping data.
        Err(_) => true,
    }
}

/// Pure window predicate: is `mtime` within `days` of `now`?
///
/// `days == 0` is treated as "no lower bound" (include everything) to avoid an
/// empty scan on a nonsensical window; the CLI clamps to a sane default before
/// calling. Kept dependency-free and pure so the date logic is unit-testable
/// without backdating files on disk.
fn is_within(mtime: SystemTime, now: SystemTime, days: u32) -> bool {
    if days == 0 {
        return true;
    }
    match now.checked_sub(Duration::from_secs(u64::from(days) * 86_400)) {
        Some(cutoff) => mtime >= cutoff,
        // Underflow (absurdly large `days`): treat as no lower bound.
        None => true,
    }
}

/// The project identifier is the first path component under `projects/`.
/// Files may live directly in a project dir or nested deeper; we take the
/// top-level project folder name.
fn project_name(projects_root: &Path, file: &Path) -> String {
    file.strip_prefix(projects_root)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Match a project against a filter. Supports plain substring matching and a
/// minimal `*` wildcard glob, tested against both the project dir name and the
/// full file path (case-sensitive, matching Claude Code's sanitized paths).
fn matches_filter(project: &str, full_path: &str, filter: &str) -> bool {
    if filter.contains('*') {
        glob_match(filter, project) || glob_match(filter, full_path)
    } else {
        project.contains(filter) || full_path.contains(filter)
    }
}

/// Tiny glob supporting only `*` (matches any run of characters, including
/// none). Anchored at both ends. Sufficient for `--project` convenience;
/// deliberately not a full glob engine (no dependency added).
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut idx = 0usize;

    // Leading segment must be a prefix (unless the pattern starts with '*').
    if let Some(first) = parts.first() {
        if !text[idx..].starts_with(first) {
            return false;
        }
        idx += first.len();
    }
    // Middle segments must appear in order.
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        match text[idx..].find(part) {
            Some(pos) => idx += pos + part.len(),
            None => return false,
        }
    }
    // Trailing segment must be a suffix (unless the pattern ends with '*').
    if let Some(last) = parts.last() {
        if last.is_empty() {
            return true;
        }
        return text[idx..].ends_with(last) && text.len() - idx >= last.len();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_basic() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("foo*", "foobar"));
        assert!(glob_match("*bar", "foobar"));
        assert!(glob_match("foo*bar", "fooXXbar"));
        assert!(glob_match("foo*bar", "foobar"));
        assert!(!glob_match("foo*bar", "fooXXbaz"));
        assert!(!glob_match("foo", "foobar"));
        assert!(glob_match("a*b*c", "aXbYc"));
    }

    #[test]
    fn substring_filter_matches_name_or_path() {
        assert!(matches_filter("my-project", "/c/x/my-project/s.jsonl", "my-project"));
        assert!(matches_filter("my-project", "/c/x/my-project/s.jsonl", "x/my-project"));
        assert!(!matches_filter("my-project", "/c/x/my-project/s.jsonl", "other"));
    }

    #[test]
    fn glob_filter_matches_project() {
        assert!(matches_filter("hypermile-web", "/p/hypermile-web/s.jsonl", "hyper*web"));
        assert!(!matches_filter("hypermile-web", "/p/hypermile-web/s.jsonl", "hyper*api"));
    }

    #[test]
    fn window_includes_recent_excludes_old() {
        let now = SystemTime::now();
        let two_days_ago = now - Duration::from_secs(2 * 86_400);
        let ten_days_ago = now - Duration::from_secs(10 * 86_400);
        // 7-day window
        assert!(is_within(two_days_ago, now, 7));
        assert!(!is_within(ten_days_ago, now, 7));
        // Exactly at the boundary is included (>=).
        let seven_days_ago = now - Duration::from_secs(7 * 86_400);
        assert!(is_within(seven_days_ago, now, 7));
    }

    #[test]
    fn window_zero_days_has_no_lower_bound() {
        let now = SystemTime::now();
        let long_ago = now - Duration::from_secs(365 * 86_400);
        assert!(is_within(long_ago, now, 0));
    }
}
