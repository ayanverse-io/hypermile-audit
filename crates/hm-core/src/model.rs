//! Defensive serde model for Claude Code transcript lines.
//!
//! Transcripts are newline-delimited JSON (`.jsonl`), one object per line, at
//! `<claude-dir>/projects/<sanitized-path>/<session-uuid>.jsonl`. Shapes vary
//! across Claude Code versions, so every field here is optional and every enum
//! tolerates unknown variants (spec 00 §4). Nothing in this module ever panics
//! on unexpected input — a line that does not deserialize is counted as a parse
//! error by the caller and skipped.

use serde::Deserialize;

/// One line of a transcript file.
///
/// `type` is the discriminator ("assistant", "user", "system", "summary",
/// unknown future values, …). We keep it as a plain `Option<String>` rather
/// than an enum so that an unrecognized `type` deserializes cleanly instead of
/// failing the whole line.
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptLine {
    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub message: Option<Message>,
    /// Sidechain marker carried on real Claude Code sub-agent entries
    /// (`"isSidechain": true`). Absent on transcripts that predate the field —
    /// the waste analyzer degrades to "unknown" when it never appears (spec 01
    /// §3.4 sub-agent multiplier).
    #[serde(rename = "isSidechain", default)]
    pub is_sidechain: Option<bool>,
}

/// The `message` payload carried by assistant/user lines.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Message {
    pub role: Option<String>,
    /// Model id (e.g. "claude-x"). Parsed for completeness of the wire shape and
    /// available to consumers; not used by the current attribution logic.
    #[allow(dead_code)]
    pub model: Option<String>,
    /// `content` is either a bare string or an array of typed blocks.
    #[serde(default)]
    pub content: Option<RawContent>,
    /// Present on assistant messages; absent elsewhere (and sometimes absent
    /// even on assistant lines — tolerate it).
    pub usage: Option<Usage>,
}

/// `content` in the wild is `"a string"` OR `[ {block}, {block} ]`.
/// `#[serde(untagged)]` tries each variant in order.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawContent {
    Text(String),
    Blocks(Vec<Block>),
}

impl RawContent {
    /// Normalize to a slice-friendly view for iteration.
    pub fn blocks(&self) -> Vec<Block> {
        match self {
            RawContent::Text(s) => vec![Block::synthetic_text(s.clone())],
            RawContent::Blocks(b) => b.clone(),
        }
    }
}

/// A single content block. One struct covers every block kind (`text`,
/// `thinking`, `tool_use`, `tool_result`, and unknown types) — the fields that
/// do not apply are simply `None`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Block {
    #[serde(rename = "type")]
    pub ty: Option<String>,

    // text / thinking blocks
    pub text: Option<String>,
    pub thinking: Option<String>,

    // tool_use blocks
    pub id: Option<String>,
    pub name: Option<String>,
    /// `tool_use` input arguments. Only the fields the analyzer needs are typed;
    /// every other argument (`command`, `pattern`, …) is ignored by serde. Used
    /// to recover the `file_path` a `Read` call targeted for repeated-read
    /// detection (spec 01 §3.4).
    #[serde(default)]
    pub input: Option<ToolInput>,

    // tool_result blocks
    pub tool_use_id: Option<String>,
    /// tool_result content is itself either a string or an array of blocks.
    #[serde(default)]
    pub content: Option<RawContent>,
}

impl Block {
    /// Build a text block from a bare string `content` field.
    fn synthetic_text(text: String) -> Self {
        Block {
            ty: Some("text".to_string()),
            text: Some(text),
            ..Block::default()
        }
    }
}

/// The `input` arguments object of a `tool_use` block. Only `file_path` is
/// typed (needed for repeated-read detection); serde ignores all other keys, so
/// arbitrary tool arguments deserialize without error.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolInput {
    pub file_path: Option<String>,
}

/// API-reported token usage. All fields default to 0 so a partial `usage`
/// object (e.g. missing cache fields) still deserializes.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    /// Total context processed by this API turn: fresh input + generated output
    /// + cache reads + cache writes (spec 01 §4 "Total context processed").
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_read_input_tokens
            + self.cache_creation_input_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_array_content_with_tool_use_and_usage() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","model":"claude-x","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":12,"output_tokens":345,"cache_read_input_tokens":45000,"cache_creation_input_tokens":2000}}}"#;
        let parsed: TranscriptLine = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.ty.as_deref(), Some("assistant"));
        let msg = parsed.message.unwrap();
        let usage = msg.usage.unwrap();
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.total(), 12 + 345 + 45000 + 2000);
        let blocks = msg.content.unwrap().blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].name.as_deref(), Some("Bash"));
        assert_eq!(blocks[1].id.as_deref(), Some("toolu_01"));
    }

    #[test]
    fn parses_string_content() {
        let line = r#"{"type":"user","message":{"role":"user","content":"just a string prompt"}}"#;
        let parsed: TranscriptLine = serde_json::from_str(line).unwrap();
        let blocks = parsed.message.unwrap().content.unwrap().blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].ty.as_deref(), Some("text"));
        assert_eq!(blocks[0].text.as_deref(), Some("just a string prompt"));
    }

    #[test]
    fn tolerates_missing_usage_and_unknown_type() {
        let line = r#"{"type":"some_future_kind","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"output text"}]}}"#;
        let parsed: TranscriptLine = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.ty.as_deref(), Some("some_future_kind"));
        let msg = parsed.message.unwrap();
        assert!(msg.usage.is_none());
        let blocks = msg.content.unwrap().blocks();
        assert_eq!(blocks[0].tool_use_id.as_deref(), Some("toolu_01"));
        // nested tool_result string content normalizes to one text block
        assert_eq!(blocks[0].content.as_ref().unwrap().blocks()[0].text.as_deref(), Some("output text"));
    }

    #[test]
    fn missing_content_field_is_ok() {
        let line = r#"{"type":"system","message":{"role":"system"}}"#;
        let parsed: TranscriptLine = serde_json::from_str(line).unwrap();
        assert!(parsed.message.unwrap().content.is_none());
    }
}
