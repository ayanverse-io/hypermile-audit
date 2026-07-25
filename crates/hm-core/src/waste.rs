//! Waste detectors and savings estimate (spec 01 §3.4–3.5).
//!
//! This module is **pure**: it performs no file or network I/O. It consumes
//! already-parsed [`TranscriptLine`] values (the streaming/reading happens in
//! [`crate::analyze_waste`]) and produces a [`WasteReport`]. Every detector is a
//! small, independently testable function over that parsed data.
//!
//! ## Detectors
//! 1. **Repeated file reads** — the same `file_path` read by more than one `Read`
//!    tool call in a session; every read beyond the first is duplicate-read
//!    waste. Top offender paths are reported with their read counts.
//! 2. **Giant JSON blobs** — `tool_result` text that parses as JSON and is either
//!    an array with more than 20 items or larger than 8 KB. Estimated 70 %
//!    compressible.
//! 3. **Log-noise** — `tool_result` with more than 50 lines where more than 30 %
//!    of lines are near-duplicates after normalization (timestamps / hex / UUIDs
//!    / numbers stripped). Estimated 60 % compressible.
//! 4. **Huge single outputs** — any `tool_result` larger than 20 KB; the ten
//!    largest are listed with tool name and project (informational).
//! 5. **Sub-agent share** — share of token burn attributed to sidechain entries
//!    (`isSidechain: true`). Degrades to [`SubAgentShare::Unknown`] when no entry
//!    in the window carries the marker.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::estimate::estimate_tokens;
use crate::model::{RawContent, TranscriptLine};
use crate::scan::collect_text;

// ---------------------------------------------------------------------------
// Tunables (spec 01 §3.4–3.5). Boundaries are strict "greater-than" throughout:
// exactly-at-threshold does NOT trigger.
// ---------------------------------------------------------------------------

/// A JSON array must exceed this many items to count as a giant blob (`> 20`).
const JSON_ARRAY_ITEM_THRESHOLD: usize = 20;
/// JSON text must exceed this many bytes to count as a giant blob (`> 8 KB`).
const JSON_BYTE_THRESHOLD: usize = 8 * 1024;
/// A `tool_result` must have more than this many lines to be log-noise (`> 50`).
const LOG_MIN_LINES: usize = 50;
/// Near-duplicate line fraction above which a result is log-noise (`> 0.30`).
const LOG_DUP_RATIO: f64 = 0.30;
/// A `tool_result` larger than this many bytes is a "huge output" (`> 20 KB`).
const HUGE_OUTPUT_BYTES: usize = 20 * 1024;

/// Fraction of a giant JSON blob assumed compressible (70 %).
const JSON_COMPRESSIBLE: f64 = 0.70;
/// Fraction of a log-noise blob assumed compressible (60 %).
const LOG_COMPRESSIBLE: f64 = 0.60;

/// Savings are capped at this fraction of tool-output tokens (65 %).
const SAVINGS_TOOL_CAP: f64 = 0.65;
/// Low end of the reported range: estimate − 15 %.
const SAVINGS_LOW_FACTOR: f64 = 0.85;
/// High end of the reported range: estimate + 10 %.
const SAVINGS_HIGH_FACTOR: f64 = 1.10;
/// The reported percentages are clamped to `[0, 65]`.
const SAVINGS_PCT_CAP: f64 = 65.0;

/// How many entries to keep in each "top offenders" list.
const TOP_N: usize = 10;

// ---------------------------------------------------------------------------
// Public report types (Serialize-ready for the CLI's `--json` schema, spec §4).
// ---------------------------------------------------------------------------

/// The complete waste analysis for a scan window. Returned by
/// [`crate::analyze_waste`].
#[derive(Debug, Clone, Serialize)]
pub struct WasteReport {
    pub repeated_reads: RepeatedReads,
    pub json_blobs: JsonBlobs,
    pub log_noise: LogNoise,
    /// The ten largest single `tool_result` outputs, descending by byte size.
    pub huge_outputs: Vec<HugeOutput>,
    pub sub_agent: SubAgentShare,
    /// The headline savings estimate as a percentage range.
    pub savings: SavingsRange,
    /// Estimated tokens across all `tool_result` output (the savings-cap base:
    /// savings are capped at 65 % of this).
    pub tool_output_tokens: u64,
    /// Single-copy estimated content total: the sum of chars-estimated tokens for
    /// every text-bearing block (tool_result + assistant text/thinking + user
    /// text), each counted **once**. This is the denominator the [`SavingsRange`]
    /// is computed against (spec 01 §3.5 denominator correction).
    pub content_tokens: u64,
    /// Total non-cache-read billed tokens (input + output + cache-creation), summed
    /// across every turn. This is real billed burn — it re-counts blocks on every
    /// re-send — and is **not** the savings denominator; it is retained so the CLI
    /// can translate the savings percentage into "extra days before your weekly
    /// cap" against real billed volume.
    pub non_cache_tokens: u64,
}

/// Repeated-file-read waste (detector 1).
#[derive(Debug, Clone, Default, Serialize)]
pub struct RepeatedReads {
    /// Tokens spent re-reading files already read earlier in the same session.
    pub wasted_tokens: u64,
    /// Number of distinct file paths read more than once in some session.
    pub file_count: usize,
    /// Worst offenders, descending by wasted tokens (top ten).
    pub offenders: Vec<ReadOffender>,
}

/// One repeatedly-read file. `reads` counts every read that participated in a
/// repeat (aggregated across sessions); `wasted_tokens` excludes the first read
/// of each session.
#[derive(Debug, Clone, Serialize)]
pub struct ReadOffender {
    pub path: String,
    pub reads: u32,
    pub wasted_tokens: u64,
}

/// Giant-JSON-blob waste (detector 2).
#[derive(Debug, Clone, Default, Serialize)]
pub struct JsonBlobs {
    pub count: u64,
    /// Estimated compressible tokens (70 % of the blobs' token estimate).
    pub wasted_tokens: u64,
    /// Up to ten example blobs, descending by byte size.
    pub samples: Vec<BlobSample>,
}

/// Why a `tool_result` was flagged as a giant JSON blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BlobKind {
    /// A JSON array with more than 20 items.
    LargeArray,
    /// JSON text larger than 8 KB.
    Oversize,
}

/// One giant JSON blob example (never includes content — tool + project + size).
#[derive(Debug, Clone, Serialize)]
pub struct BlobSample {
    pub tool: String,
    pub project: String,
    pub bytes: u64,
    pub kind: BlobKind,
}

/// Log-noise waste (detector 3).
#[derive(Debug, Clone, Default, Serialize)]
pub struct LogNoise {
    pub count: u64,
    /// Estimated compressible tokens (60 % of the noisy blobs' token estimate).
    pub wasted_tokens: u64,
    /// Up to ten example results, descending by line count.
    pub samples: Vec<NoiseSample>,
}

/// One log-noise example (tool + project + shape only, never content).
#[derive(Debug, Clone, Serialize)]
pub struct NoiseSample {
    pub tool: String,
    pub project: String,
    pub lines: u32,
    /// Percentage of near-duplicate lines after normalization (0–100).
    pub duplicate_pct: u8,
}

/// One huge single output (detector 4).
#[derive(Debug, Clone, Serialize)]
pub struct HugeOutput {
    pub tool: String,
    pub project: String,
    pub bytes: u64,
    pub tokens: u64,
}

/// Sub-agent burn share (detector 5). `Unknown` when no `isSidechain` marker was
/// seen anywhere in the window, so the share cannot be reliably computed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SubAgentShare {
    Unknown,
    Known {
        sidechain_tokens: u64,
        total_tokens: u64,
        /// Sidechain tokens as a percentage of total burn (0–100).
        pct: f64,
    },
}

/// The headline savings estimate as a percentage range.
///
/// # Formula (spec 01 §3.5, incl. the 2026-07-22 denominator correction)
/// Let `raw` be the sum of the three token-valued detector estimates
/// (repeated-read + giant-JSON + log-noise), all counted **once** per block.
/// Then:
///
/// ```text
/// capped   = min(raw, 0.65 * tool_output_tokens)        // token-level cap
/// est_pct  = 100 * capped / content_tokens              // 0 if denominator is 0
/// low  = clamp(est_pct * 0.85, 0, 65)                   // estimate − 15 %
/// high = clamp(est_pct * 1.10, 0, 65)                   // estimate + 10 %
/// ```
///
/// # Why the denominator is single-copy content, not billed usage
/// The detector numerator counts each block's content **once**. Real billed
/// `usage` re-counts every earlier block on every subsequent request of a
/// session, so a long session's billed total is a large multiple of its
/// single-copy content (observed ~40× on real data). Dividing once-counted waste
/// by many-times-counted billed usage understated savings to ~1 %. The correct
/// proxy is the **single-copy ratio**: removing X % of the conversation's content
/// shrinks each subsequent request by ~X % of those blocks, so the fraction of
/// single-copy content that is compressible *is* the fraction of billed volume
/// saved. Hence `content_tokens` (single-copy) is the denominator; billed totals
/// are only applied afterward to express the result as saved days.
///
/// Both `low` and `high` are percentages in `[0.0, 65.0]`. Huge-output and
/// sub-agent findings are informational and do **not** feed this estimate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SavingsRange {
    pub low: f64,
    pub high: f64,
}

// ---------------------------------------------------------------------------
// Per-session collector. Fed one parsed line at a time by `analyze_waste`.
// ---------------------------------------------------------------------------

/// Accumulates waste evidence for a single transcript file (one session).
///
/// Repeated-read detection is per-session (re-reading a file in two *different*
/// sessions is not a repeat), so the join tables live here and are resolved in
/// [`SessionWaste::finish`].
pub(crate) struct SessionWaste {
    project: String,
    /// `tool_use_id` -> originating tool name (session-scoped join table).
    tool_names: HashMap<String, String>,
    /// `tool_use_id` -> `file_path` for `Read` tool calls.
    read_paths: HashMap<String, String>,
    /// `file_path` -> per-read token estimates, in read order.
    path_reads: HashMap<String, Vec<u64>>,

    // Findings surfaced immediately (not per-session-scoped).
    json_blobs: Vec<BlobSample>,
    json_wasted: u64,
    noise: Vec<NoiseSample>,
    noise_wasted: u64,
    huge: Vec<HugeOutput>,

    // Running token totals.
    tool_output_tokens: u64,
    content_tokens: u64,
    non_cache_tokens: u64,
    total_burn: u64,
    sidechain_burn: u64,
    saw_sidechain_field: bool,
}

impl SessionWaste {
    pub(crate) fn new(project: String) -> Self {
        SessionWaste {
            project,
            tool_names: HashMap::new(),
            read_paths: HashMap::new(),
            path_reads: HashMap::new(),
            json_blobs: Vec::new(),
            json_wasted: 0,
            noise: Vec::new(),
            noise_wasted: 0,
            huge: Vec::new(),
            tool_output_tokens: 0,
            content_tokens: 0,
            non_cache_tokens: 0,
            total_burn: 0,
            sidechain_burn: 0,
            saw_sidechain_field: false,
        }
    }

    /// Fold one parsed transcript line into the session's accounting.
    pub(crate) fn observe(&mut self, line: &TranscriptLine) {
        // Burn accounting (sub-agent share + savings denominator).
        if let Some(usage) = line.message.as_ref().and_then(|m| m.usage.as_ref()) {
            let total = usage.total();
            self.total_burn += total;
            self.non_cache_tokens += total - usage.cache_read_input_tokens;
        }
        if let Some(flag) = line.is_sidechain {
            self.saw_sidechain_field = true;
            if flag {
                if let Some(usage) = line.message.as_ref().and_then(|m| m.usage.as_ref()) {
                    self.sidechain_burn += usage.total();
                }
            }
        }

        let Some(content) = line.message.as_ref().and_then(|m| m.content.as_ref()) else {
            return;
        };

        // Register tool_use blocks first so tool_results in this same message can
        // join (harmless when the result is in a later message, which is typical).
        for block in content.blocks() {
            if block.ty.as_deref() == Some("tool_use") {
                if let (Some(id), Some(name)) = (&block.id, &block.name) {
                    self.tool_names.insert(id.clone(), name.clone());
                    if name == "Read" {
                        if let Some(path) = block.input.as_ref().and_then(|i| i.file_path.clone()) {
                            self.read_paths.insert(id.clone(), path);
                        }
                    }
                }
            }
        }

        // Attribute blocks. `tool_result` feeds the detectors; every text-bearing
        // block (tool_result / text / thinking) also feeds the single-copy content
        // total that the savings ratio is measured against — the same
        // once-per-block accounting the detector numerator uses (see
        // [`SavingsRange`]). Assistant `text`/`thinking` and user `text` are
        // counted once here; `tool_use` inputs are not attributed (matching
        // `scan`).
        for block in content.blocks() {
            match block.ty.as_deref() {
                Some("tool_result") => {
                    if let Some(inner) = block.content.as_ref() {
                        self.observe_tool_result(block.tool_use_id.as_deref(), inner);
                    }
                }
                Some("text") => {
                    if let Some(t) = &block.text {
                        self.content_tokens += estimate_tokens(t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = &block.thinking {
                        self.content_tokens += estimate_tokens(t);
                    }
                }
                _ => {}
            }
        }
    }

    fn observe_tool_result(&mut self, tool_use_id: Option<&str>, content: &RawContent) {
        let text = collect_text(content);
        if text.is_empty() {
            return;
        }
        let tokens = estimate_tokens(&text);
        let bytes = text.len();
        self.tool_output_tokens += tokens;
        // tool_result text is part of the single-copy content total too.
        self.content_tokens += tokens;

        let tool_name = tool_use_id
            .and_then(|id| self.tool_names.get(id))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        // Detector 1: repeated reads — record this read's cost against its path.
        if let Some(id) = tool_use_id {
            if let Some(path) = self.read_paths.get(id) {
                self.path_reads.entry(path.clone()).or_default().push(tokens);
            }
        }

        // Detector 2: giant JSON blob.
        if let Some(kind) = classify_json_blob(&text) {
            self.json_wasted += compressible(tokens, JSON_COMPRESSIBLE);
            self.json_blobs.push(BlobSample {
                tool: tool_name.clone(),
                project: self.project.clone(),
                bytes: bytes as u64,
                kind,
            });
        }

        // Detector 3: log-noise.
        if let Some((lines, dup_pct)) = log_noise_metrics(&text) {
            self.noise_wasted += compressible(tokens, LOG_COMPRESSIBLE);
            self.noise.push(NoiseSample {
                tool: tool_name.clone(),
                project: self.project.clone(),
                lines,
                duplicate_pct: dup_pct,
            });
        }

        // Detector 4: huge single output.
        if bytes > HUGE_OUTPUT_BYTES {
            self.huge.push(HugeOutput {
                tool: tool_name,
                project: self.project.clone(),
                bytes: bytes as u64,
                tokens,
            });
        }
    }

    /// Resolve per-session state and merge into the global accumulator.
    fn finish(self, acc: &mut WasteAccumulator) {
        // Repeated reads: every read beyond the first (per path, per session).
        for (path, reads) in self.path_reads {
            if reads.len() < 2 {
                continue;
            }
            let wasted: u64 = reads.iter().skip(1).sum();
            let entry = acc
                .read_offenders
                .entry(path)
                .or_insert((0u32, 0u64));
            entry.0 += reads.len() as u32;
            entry.1 += wasted;
            acc.repeated_wasted += wasted;
        }

        acc.json_blobs.extend(self.json_blobs);
        acc.json_wasted += self.json_wasted;
        acc.noise.extend(self.noise);
        acc.noise_wasted += self.noise_wasted;
        acc.huge.extend(self.huge);

        acc.tool_output_tokens += self.tool_output_tokens;
        acc.content_tokens += self.content_tokens;
        acc.non_cache_tokens += self.non_cache_tokens;
        acc.total_burn += self.total_burn;
        acc.sidechain_burn += self.sidechain_burn;
        acc.saw_sidechain_field |= self.saw_sidechain_field;
    }
}

// ---------------------------------------------------------------------------
// Cross-session accumulator.
// ---------------------------------------------------------------------------

/// Merges [`SessionWaste`] results across every scanned file and produces the
/// final [`WasteReport`].
pub(crate) struct WasteAccumulator {
    /// `file_path` -> (aggregate read count, aggregate wasted tokens).
    read_offenders: HashMap<String, (u32, u64)>,
    repeated_wasted: u64,
    json_blobs: Vec<BlobSample>,
    json_wasted: u64,
    noise: Vec<NoiseSample>,
    noise_wasted: u64,
    huge: Vec<HugeOutput>,
    tool_output_tokens: u64,
    content_tokens: u64,
    non_cache_tokens: u64,
    total_burn: u64,
    sidechain_burn: u64,
    saw_sidechain_field: bool,
}

impl WasteAccumulator {
    pub(crate) fn new() -> Self {
        WasteAccumulator {
            read_offenders: HashMap::new(),
            repeated_wasted: 0,
            json_blobs: Vec::new(),
            json_wasted: 0,
            noise: Vec::new(),
            noise_wasted: 0,
            huge: Vec::new(),
            tool_output_tokens: 0,
            content_tokens: 0,
            non_cache_tokens: 0,
            total_burn: 0,
            sidechain_burn: 0,
            saw_sidechain_field: false,
        }
    }

    pub(crate) fn add_session(&mut self, session: SessionWaste) {
        session.finish(self);
    }

    pub(crate) fn finish(mut self) -> WasteReport {
        // Repeated reads → sorted offenders (wasted desc, reads desc, path asc).
        let file_count = self.read_offenders.len();
        let mut offenders: Vec<ReadOffender> = self
            .read_offenders
            .drain()
            .map(|(path, (reads, wasted))| ReadOffender {
                path,
                reads,
                wasted_tokens: wasted,
            })
            .collect();
        offenders.sort_by(|a, b| {
            b.wasted_tokens
                .cmp(&a.wasted_tokens)
                .then(b.reads.cmp(&a.reads))
                .then_with(|| a.path.cmp(&b.path))
        });
        offenders.truncate(TOP_N);

        // JSON blob samples: largest first, keep top ten.
        self.json_blobs.sort_by_key(|s| Reverse(s.bytes));
        let json_count = self.json_blobs.len() as u64;
        self.json_blobs.truncate(TOP_N);

        // Log-noise samples: most lines first, keep top ten.
        self.noise.sort_by_key(|s| Reverse(s.lines));
        let noise_count = self.noise.len() as u64;
        self.noise.truncate(TOP_N);

        // Huge outputs: largest first, keep top ten.
        self.huge.sort_by_key(|h| Reverse(h.bytes));
        self.huge.truncate(TOP_N);

        let sub_agent = if self.saw_sidechain_field {
            let pct = if self.total_burn > 0 {
                100.0 * self.sidechain_burn as f64 / self.total_burn as f64
            } else {
                0.0
            };
            SubAgentShare::Known {
                sidechain_tokens: self.sidechain_burn,
                total_tokens: self.total_burn,
                pct,
            }
        } else {
            SubAgentShare::Unknown
        };

        let savings = compute_savings(
            self.repeated_wasted + self.json_wasted + self.noise_wasted,
            self.tool_output_tokens,
            self.content_tokens,
        );

        WasteReport {
            repeated_reads: RepeatedReads {
                wasted_tokens: self.repeated_wasted,
                file_count,
                offenders,
            },
            json_blobs: JsonBlobs {
                count: json_count,
                wasted_tokens: self.json_wasted,
                samples: self.json_blobs,
            },
            log_noise: LogNoise {
                count: noise_count,
                wasted_tokens: self.noise_wasted,
                samples: self.noise,
            },
            huge_outputs: self.huge,
            sub_agent,
            savings,
            tool_output_tokens: self.tool_output_tokens,
            content_tokens: self.content_tokens,
            non_cache_tokens: self.non_cache_tokens,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure detector helpers (unit-tested directly).
// ---------------------------------------------------------------------------

/// `fraction` of `tokens`, rounded to the nearest whole token.
fn compressible(tokens: u64, fraction: f64) -> u64 {
    (tokens as f64 * fraction).round() as u64
}

/// Classify a `tool_result` text as a giant JSON blob, if it is one. Returns
/// `None` when the text does not parse as JSON or is under both thresholds.
///
/// Boundaries are strict: exactly 20 array items or exactly 8 KB do **not**
/// qualify.
fn classify_json_blob(text: &str) -> Option<BlobKind> {
    let trimmed = text.trim();
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if let Some(arr) = value.as_array() {
        if arr.len() > JSON_ARRAY_ITEM_THRESHOLD {
            return Some(BlobKind::LargeArray);
        }
    }
    if text.len() > JSON_BYTE_THRESHOLD {
        return Some(BlobKind::Oversize);
    }
    None
}

/// Compute log-noise metrics for a `tool_result` text. Returns
/// `Some((line_count, duplicate_pct))` when the text has more than 50 lines and
/// more than 30 % near-duplicate lines after normalization; `None` otherwise.
fn log_noise_metrics(text: &str) -> Option<(u32, u8)> {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    if n <= LOG_MIN_LINES {
        return None;
    }
    let mut unique: HashSet<String> = HashSet::with_capacity(n);
    for line in &lines {
        unique.insert(normalize_line(line));
    }
    let duplicates = n - unique.len();
    let ratio = duplicates as f64 / n as f64;
    if ratio > LOG_DUP_RATIO {
        let pct = (ratio * 100.0).round().min(100.0) as u8;
        Some((n as u32, pct))
    } else {
        None
    }
}

/// Normalize a log line so that lines differing only in volatile tokens
/// (timestamps, hex addresses, UUIDs, numbers) collapse to the same string.
///
/// Hand-rolled char scanner (no regex dependency): lowercases letters, replaces
/// UUIDs with `<uuid>`, `0x…` and long hex runs with `<hex>`, digit runs with
/// `<n>`, and collapses whitespace runs to a single space.
fn normalize_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    let mut prev_space = false;

    while i < len {
        let c = chars[i];

        // UUID: 8-4-4-4-12 hex groups separated by '-'.
        if let Some(adv) = match_uuid(&chars, i) {
            out.push_str("<uuid>");
            i += adv;
            prev_space = false;
            continue;
        }

        // `0x` / `0X` hex literal.
        if c == '0' && i + 1 < len && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
            let mut j = i + 2;
            while j < len && chars[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > i + 2 {
                out.push_str("<hex>");
                i = j;
                prev_space = false;
                continue;
            }
        }

        // Standalone long hex run (>= 6 hex digits, at least one letter so plain
        // numbers stay `<n>`): matches addresses / SHAs.
        if c.is_ascii_hexdigit() {
            let mut j = i;
            let mut has_alpha = false;
            while j < len && chars[j].is_ascii_hexdigit() {
                if chars[j].is_ascii_alphabetic() {
                    has_alpha = true;
                }
                j += 1;
            }
            if j - i >= 6 && has_alpha {
                out.push_str("<hex>");
                i = j;
                prev_space = false;
                continue;
            }
        }

        // Digit run.
        if c.is_ascii_digit() {
            while i < len && chars[i].is_ascii_digit() {
                i += 1;
            }
            out.push_str("<n>");
            prev_space = false;
            continue;
        }

        // Whitespace run → single space.
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            i += 1;
            continue;
        }

        out.extend(c.to_lowercase());
        prev_space = false;
        i += 1;
    }

    out.trim().to_string()
}

/// If a canonical `8-4-4-4-12` hex UUID starts at `chars[i]`, return its length
/// in chars (36); otherwise `None`.
fn match_uuid(chars: &[char], i: usize) -> Option<usize> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut pos = i;
    for (g, &group_len) in GROUPS.iter().enumerate() {
        for _ in 0..group_len {
            let c = chars.get(pos)?;
            if !c.is_ascii_hexdigit() {
                return None;
            }
            pos += 1;
        }
        if g < GROUPS.len() - 1 {
            if chars.get(pos) != Some(&'-') {
                return None;
            }
            pos += 1;
        }
    }
    Some(pos - i)
}

/// Compute the [`SavingsRange`] from the raw detector-estimate sum, the
/// tool-output cap base, and the single-copy content denominator. See
/// [`SavingsRange`] for the exact formula and the denominator rationale.
fn compute_savings(raw_estimate: u64, tool_output_tokens: u64, content_tokens: u64) -> SavingsRange {
    let cap = (tool_output_tokens as f64 * SAVINGS_TOOL_CAP) as u64;
    let capped = raw_estimate.min(cap);
    let est_pct = if content_tokens > 0 {
        100.0 * capped as f64 / content_tokens as f64
    } else {
        0.0
    };
    SavingsRange {
        low: (est_pct * SAVINGS_LOW_FACTOR).clamp(0.0, SAVINGS_PCT_CAP),
        high: (est_pct * SAVINGS_HIGH_FACTOR).clamp(0.0, SAVINGS_PCT_CAP),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe_str(session: &mut SessionWaste, jsonl: &str) {
        for line in jsonl.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: TranscriptLine = serde_json::from_str(line).unwrap();
            session.observe(&parsed);
        }
    }

    fn report_from(project: &str, jsonl: &str) -> WasteReport {
        let mut acc = WasteAccumulator::new();
        let mut sess = SessionWaste::new(project.to_string());
        observe_str(&mut sess, jsonl);
        acc.add_session(sess);
        acc.finish()
    }

    // ---- normalize_line -------------------------------------------------

    #[test]
    fn normalize_collapses_numbers_and_timestamps() {
        let a = normalize_line("2026-07-22 10:00:01 INFO request 4821 done");
        let b = normalize_line("2026-07-22 11:59:59 INFO request 9310 done");
        assert_eq!(a, b, "timestamp + counter lines should normalize equal");
    }

    #[test]
    fn normalize_handles_hex_and_uuid() {
        let a = normalize_line("obj 0xDEADBEEF id 550e8400-e29b-41d4-a716-446655440000");
        let b = normalize_line("obj 0x0badf00d id 550e8400-e29b-41d4-a716-000000000000");
        assert_eq!(a, b);
        assert!(a.contains("<hex>") && a.contains("<uuid>"));
    }

    #[test]
    fn normalize_keeps_distinct_lines_distinct() {
        assert_ne!(normalize_line("starting server"), normalize_line("stopping server"));
    }

    #[test]
    fn uuid_matcher_rejects_non_uuid() {
        let chars: Vec<char> = "not-a-uuid-here".chars().collect();
        assert_eq!(match_uuid(&chars, 0), None);
    }

    // ---- classify_json_blob boundaries ---------------------------------

    #[test]
    fn json_array_of_exactly_20_is_not_a_blob() {
        let arr = format!("[{}]", vec!["1"; 20].join(","));
        assert_eq!(classify_json_blob(&arr), None);
    }

    #[test]
    fn json_array_of_21_is_a_large_array() {
        let arr = format!("[{}]", vec!["1"; 21].join(","));
        assert_eq!(classify_json_blob(&arr), Some(BlobKind::LargeArray));
    }

    #[test]
    fn json_object_over_8kb_is_oversize() {
        let big = format!(r#"{{"k":"{}"}}"#, "a".repeat(9000));
        assert_eq!(classify_json_blob(&big), Some(BlobKind::Oversize));
    }

    #[test]
    fn non_json_text_is_not_a_blob() {
        assert_eq!(classify_json_blob("just some log output, not json"), None);
    }

    #[test]
    fn small_json_is_not_a_blob() {
        assert_eq!(classify_json_blob(r#"{"a":1,"b":2}"#), None);
    }

    // ---- log_noise_metrics boundaries ----------------------------------

    #[test]
    fn exactly_50_lines_is_not_log_noise() {
        let text = std::iter::repeat("same line").take(50).collect::<Vec<_>>().join("\n");
        assert_eq!(log_noise_metrics(&text), None);
    }

    #[test]
    fn fifty_one_identical_lines_is_log_noise() {
        let text = std::iter::repeat("same line").take(51).collect::<Vec<_>>().join("\n");
        let (lines, dup) = log_noise_metrics(&text).expect("should be noise");
        assert_eq!(lines, 51);
        assert!(dup >= 98, "near-100% duplicate, got {dup}");
    }

    #[test]
    fn diverse_lines_are_not_log_noise() {
        // Numbered lines all normalize to "unique event kind <n> name" → noise.
        let numbered = (0..60)
            .map(|i| format!("unique event kind {i} name"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(log_noise_metrics(&numbered).is_some(), "numbered lines normalize identical");

        // Genuinely distinct lines (distinct lengths, no digits) → not noise.
        let distinct = (0..60)
            .map(|i| "x".repeat(i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(log_noise_metrics(&distinct), None, "distinct lines are not noise");
    }

    // ---- compute_savings ------------------------------------------------

    #[test]
    fn savings_zero_when_nothing_scanned() {
        let s = compute_savings(0, 0, 0);
        assert_eq!(s.low, 0.0);
        assert_eq!(s.high, 0.0);
    }

    #[test]
    fn savings_capped_at_65_percent_of_tool_output() {
        // raw estimate is huge, tool output small → capped to 0.65 * tool_output.
        let s = compute_savings(1_000_000, 1000, 1000);
        // capped = 650 tokens; est_pct = 65% of non_cache(1000) = 65%.
        // low = 65 * 0.85 = 55.25 ; high = 65 * 1.10 = 71.5 -> clamped 65.
        assert!((s.low - 55.25).abs() < 1e-6, "low was {}", s.low);
        assert_eq!(s.high, 65.0, "high clamps to 65");
    }

    #[test]
    fn savings_range_is_low_below_high_in_normal_case() {
        // raw 100 tokens, tool output 1000 (cap 650 not binding), non_cache 1000.
        let s = compute_savings(100, 1000, 1000);
        // est_pct = 10% ; low = 8.5 ; high = 11.0
        assert!((s.low - 8.5).abs() < 1e-6);
        assert!((s.high - 11.0).abs() < 1e-6);
    }

    // ---- end-to-end detector wiring ------------------------------------

    #[test]
    fn repeated_reads_counts_beyond_first() {
        // Same path read three times; each result ~ same tokens.
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"src/big.ts"}}]}}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"r1","content":"aaaaaaaaaaaaaaaa"}]}}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"r2","name":"Read","input":{"file_path":"src/big.ts"}}]}}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"r2","content":"aaaaaaaaaaaaaaaa"}]}}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"r3","name":"Read","input":{"file_path":"src/big.ts"}}]}}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"r3","content":"aaaaaaaaaaaaaaaa"}]}}"#,
        );
        let report = report_from("proj", jsonl);
        // 16 chars -> 4 tokens per read; first free, two duplicates -> 8 tokens.
        assert_eq!(report.repeated_reads.wasted_tokens, 8);
        assert_eq!(report.repeated_reads.file_count, 1);
        assert_eq!(report.repeated_reads.offenders[0].path, "src/big.ts");
        assert_eq!(report.repeated_reads.offenders[0].reads, 3);
    }

    #[test]
    fn single_read_is_not_waste() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"src/one.ts"}}]}}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"r1","content":"aaaa"}]}}"#,
        );
        let report = report_from("proj", jsonl);
        assert_eq!(report.repeated_reads.wasted_tokens, 0);
        assert_eq!(report.repeated_reads.file_count, 0);
    }

    #[test]
    fn giant_json_array_result_is_flagged() {
        let arr = format!("[{}]", vec!["\"x\""; 30].join(","));
        let jsonl = format!(
            concat!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"b1","name":"Bash","input":{{"command":"cat data.json"}}}}]}}}}"#, "\n",
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"b1","content":{}}}]}}}}"#,
            ),
            serde_json::to_string(&arr).unwrap()
        );
        let report = report_from("proj", &jsonl);
        assert_eq!(report.json_blobs.count, 1);
        assert!(report.json_blobs.wasted_tokens > 0);
        assert_eq!(report.json_blobs.samples[0].kind, BlobKind::LargeArray);
        assert_eq!(report.json_blobs.samples[0].tool, "Bash");
    }

    #[test]
    fn huge_output_is_listed_with_tool_and_project() {
        let big = "x".repeat(HUGE_OUTPUT_BYTES + 100);
        let jsonl = format!(
            concat!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"b1","name":"Bash","input":{{"command":"cat log"}}}}]}}}}"#, "\n",
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"b1","content":{}}}]}}}}"#,
            ),
            serde_json::to_string(&big).unwrap()
        );
        let report = report_from("proj-x", &jsonl);
        assert_eq!(report.huge_outputs.len(), 1);
        assert_eq!(report.huge_outputs[0].tool, "Bash");
        assert_eq!(report.huge_outputs[0].project, "proj-x");
        assert!(report.huge_outputs[0].bytes > HUGE_OUTPUT_BYTES as u64);
    }

    #[test]
    fn sub_agent_unknown_without_marker() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let report = report_from("proj", jsonl);
        assert_eq!(report.sub_agent, SubAgentShare::Unknown);
    }

    #[test]
    fn sub_agent_known_with_sidechain_marker() {
        let jsonl = concat!(
            r#"{"type":"assistant","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"main"}],"usage":{"input_tokens":100,"output_tokens":0}}}"#, "\n",
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"sub"}],"usage":{"input_tokens":40,"output_tokens":0}}}"#,
        );
        let report = report_from("proj", jsonl);
        match report.sub_agent {
            SubAgentShare::Known { sidechain_tokens, total_tokens, pct } => {
                assert_eq!(sidechain_tokens, 40);
                assert_eq!(total_tokens, 140);
                assert!((pct - (40.0 / 140.0 * 100.0)).abs() < 1e-6);
            }
            SubAgentShare::Unknown => panic!("expected Known"),
        }
    }

    #[test]
    fn empty_transcript_yields_zero_waste() {
        let report = report_from("proj", "");
        assert_eq!(report.repeated_reads.wasted_tokens, 0);
        assert_eq!(report.json_blobs.count, 0);
        assert_eq!(report.log_noise.count, 0);
        assert!(report.huge_outputs.is_empty());
        assert_eq!(report.sub_agent, SubAgentShare::Unknown);
        assert_eq!(report.savings.low, 0.0);
        assert_eq!(report.savings.high, 0.0);
        assert_eq!(report.tool_output_tokens, 0);
        assert_eq!(report.content_tokens, 0);
        assert_eq!(report.non_cache_tokens, 0);
    }

    #[test]
    fn savings_uses_single_copy_denominator_not_billed_usage() {
        // Regression for the 2026-07-22 real-data bug: a many-turn session whose
        // billed usage (cache reads/creation re-counted every turn) dwarfs its
        // single-copy content must NOT collapse savings to ~1%.
        //
        // Content: one giant, highly-compressible JSON-array tool_result (counted
        // once). Billed usage: eight assistant turns each carrying enormous
        // cache-read + cache-creation numbers — real burn, but re-sends of the
        // same content.
        let big_array = format!("[{}]", vec!["\"item\""; 60].join(","));
        let blob_line = format!(
            concat!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"b1","name":"Bash","input":{{"command":"cat data.json"}}}}]}}}}"#, "\n",
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"b1","content":{}}}]}}}}"#,
            ),
            serde_json::to_string(&big_array).unwrap()
        );
        // Eight high-usage turns: 500k cache-read + 20k cache-creation each.
        let mut jsonl = String::from(&blob_line);
        for _ in 0..8 {
            jsonl.push('\n');
            jsonl.push_str(
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":50,"output_tokens":10,"cache_read_input_tokens":500000,"cache_creation_input_tokens":20000}}}"#,
            );
        }

        let report = report_from("proj", &jsonl);

        // The scenario really does have billed >> single-copy content.
        assert!(
            report.non_cache_tokens > report.content_tokens * 20,
            "expected billed usage to dwarf single-copy content; billed={}, content={}",
            report.non_cache_tokens,
            report.content_tokens
        );
        // The blob is ~70% compressible; capped at 65% of tool output; measured
        // against single-copy content it lands in a sane high band, NOT ~1%.
        assert!(
            report.savings.high >= 20.0 && report.savings.high <= 65.0,
            "savings.high out of sane band: {}",
            report.savings.high
        );
        assert!(
            report.savings.low >= 20.0,
            "savings.low collapsed (regression): {}",
            report.savings.low
        );
    }
}
