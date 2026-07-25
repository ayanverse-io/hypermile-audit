//! Per-session parsing and token attribution (spec 01 §3.2–3.3).
//!
//! Files are streamed line-by-line (never slurped whole) so a 500 MB transcript
//! tree stays within the CLI's <5 s budget. Malformed lines increment a counter
//! and are skipped; they never abort the file or the run.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::categorize::{category_for_tool, Category};
use crate::estimate::estimate_tokens;
use crate::model::{RawContent, TranscriptLine, Usage};

/// The token accounting produced by scanning one transcript file (one session).
#[derive(Debug, Default, Clone)]
pub(crate) struct FileScan {
    /// Real API `usage` totals summed across assistant turns (input + output +
    /// cache read + cache creation).
    pub usage_total: u64,
    /// Real `cache_read_input_tokens` summed across assistant turns.
    pub cache_read: u64,
    /// Estimated tokens per category (chars-heuristic).
    pub categories: HashMap<Category, u64>,
    /// Lines that failed to parse in this file.
    pub parse_errors: u64,
    /// Whether any usable content was seen (a session with zero parsable lines
    /// still counts as a session, but this helps tests).
    pub lines_seen: u64,
}

impl FileScan {
    fn add_category(&mut self, category: Category, tokens: u64) {
        *self.categories.entry(category).or_insert(0) += tokens;
    }
}

/// Scan a single transcript file from disk. A file that cannot be opened is
/// reported as a single parse error (the run continues).
pub(crate) fn scan_file(path: &Path) -> FileScan {
    match File::open(path) {
        Ok(file) => scan_reader(BufReader::new(file)),
        Err(_) => FileScan {
            parse_errors: 1,
            ..FileScan::default()
        },
    }
}

/// Scan transcript lines from any buffered reader. This is the testable core:
/// integration tests point it at fixture files, unit tests at in-memory data.
pub(crate) fn scan_reader<R: BufRead>(reader: R) -> FileScan {
    let mut scan = FileScan::default();
    // Session-scoped join table: tool_use_id -> originating tool name.
    let mut tool_names: HashMap<String, String> = HashMap::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                scan.parse_errors += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        scan.lines_seen += 1;

        let parsed: TranscriptLine = match serde_json::from_str(&line) {
            Ok(p) => p,
            Err(_) => {
                scan.parse_errors += 1;
                continue;
            }
        };

        attribute_line(&parsed, &mut scan, &mut tool_names);
    }

    scan
}

/// Apply one parsed line to the running accounting.
fn attribute_line(
    line: &TranscriptLine,
    scan: &mut FileScan,
    tool_names: &mut HashMap<String, String>,
) {
    let Some(message) = &line.message else {
        return;
    };

    // Real usage numbers (headline totals) — used verbatim when present.
    if let Some(usage) = &message.usage {
        add_usage(scan, usage);
    }

    // Determine authorship for text categorization.
    let is_assistant = message.role.as_deref() == Some("assistant")
        || line.ty.as_deref() == Some("assistant");

    let Some(content) = &message.content else {
        return;
    };

    // First pass: register any tool_use blocks so later tool_results in the
    // same message (or later messages) can join by id. In practice tool_use and
    // its tool_result are in separate messages, but registering first within a
    // message is harmless and order-robust.
    for block in content.blocks() {
        if block.ty.as_deref() == Some("tool_use") {
            if let (Some(id), Some(name)) = (&block.id, &block.name) {
                tool_names.insert(id.clone(), name.clone());
            }
        }
    }

    // Second pass: attribute estimated tokens per block.
    for block in content.blocks() {
        match block.ty.as_deref() {
            Some("tool_use") => { /* inputs not attributed; already registered */ }
            Some("tool_result") => {
                let text = block
                    .content
                    .as_ref()
                    .map(collect_text)
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let tool_name = block
                    .tool_use_id
                    .as_ref()
                    .and_then(|id| tool_names.get(id))
                    .map(String::as_str);
                let category = category_for_tool(tool_name);
                scan.add_category(category, estimate_tokens(&text));
            }
            Some("thinking") => {
                if let Some(t) = &block.thinking {
                    scan.add_category(Category::ModelOutput, estimate_tokens(t));
                }
            }
            Some("text") => {
                if let Some(t) = &block.text {
                    let category = if is_assistant {
                        Category::ModelOutput
                    } else {
                        Category::YourPrompts
                    };
                    scan.add_category(category, estimate_tokens(t));
                }
            }
            // Unknown / future block types: ignored, not an error.
            _ => {}
        }
    }
}

fn add_usage(scan: &mut FileScan, usage: &Usage) {
    scan.usage_total += usage.total();
    scan.cache_read += usage.cache_read_input_tokens;
}

/// Flatten a `content` value (string or array of blocks) into a single text
/// string for estimation. Handles the nested `tool_result` case where inner
/// blocks are `{type:text,text:...}`.
pub(crate) fn collect_text(content: &RawContent) -> String {
    let mut out = String::new();
    for block in content.blocks() {
        if let Some(t) = &block.text {
            out.push_str(t);
        } else if let Some(t) = &block.thinking {
            out.push_str(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn scan_str(s: &str) -> FileScan {
        scan_reader(Cursor::new(s))
    }

    #[test]
    fn attributes_bash_tool_result_via_join() {
        let data = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"aaaaaaaa"}]}]}}"#,
        );
        let scan = scan_str(data);
        // 8 chars prose -> 2 tokens under Bash output.
        assert_eq!(scan.categories.get(&Category::BashOutput), Some(&2));
        assert_eq!(scan.usage_total, 15);
        assert_eq!(scan.parse_errors, 0);
    }

    #[test]
    fn unjoined_tool_result_is_other_tools() {
        let data = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"missing","content":"abcd"}]}}"#;
        let scan = scan_str(data);
        assert_eq!(scan.categories.get(&Category::OtherTools), Some(&1));
    }

    #[test]
    fn assistant_text_is_model_output_user_text_is_prompt() {
        let data = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"abcdefgh"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"abcd"}}"#,
        );
        let scan = scan_str(data);
        assert_eq!(scan.categories.get(&Category::ModelOutput), Some(&2));
        assert_eq!(scan.categories.get(&Category::YourPrompts), Some(&1));
    }

    #[test]
    fn thinking_counts_as_model_output() {
        let data = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"abcdefgh"}]}}"#;
        let scan = scan_str(data);
        assert_eq!(scan.categories.get(&Category::ModelOutput), Some(&2));
    }

    #[test]
    fn malformed_lines_are_counted_not_fatal() {
        let data = concat!(
            "not json at all\n",
            r#"{"type":"user","message":{"role":"user","content":"abcd"}}"#,
            "\n",
            "{ broken json\n",
            "\n", // blank line ignored, not an error
        );
        let scan = scan_str(data);
        assert_eq!(scan.parse_errors, 2);
        assert_eq!(scan.categories.get(&Category::YourPrompts), Some(&1));
    }

    #[test]
    fn mcp_tool_result_groups_under_mcp() {
        let data = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"mcp__srv__do","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"abcd"}]}}"#,
        );
        let scan = scan_str(data);
        assert_eq!(scan.categories.get(&Category::McpTools), Some(&1));
    }
}
