//! Token estimation heuristic (spec 00 §3).
//!
//! We deliberately avoid a tokenizer dependency. Instead we approximate:
//!   * prose:      `ceil(chars / 4.0)`
//!   * code/JSON:  `ceil(chars / 3.6)`
//!
//! The accuracy caveat is surfaced in the UI layer, not here. These estimates
//! are only ever used for *categorizing where tokens went* — the headline
//! totals come from real API `usage` numbers when present (see `attribute`).

/// Divisor for prose-like text.
const PROSE_DIVISOR: f64 = 4.0;
/// Divisor for code / JSON, which packs more tokens per character.
const CODE_DIVISOR: f64 = 3.6;

/// Estimate tokens for a chunk of text, auto-detecting whether it looks like
/// code/JSON. See [`estimate_tokens_with`] for the explicit-kind variant.
pub fn estimate_tokens(text: &str) -> u64 {
    estimate_tokens_with(text, looks_like_code_or_json(text))
}

/// Estimate tokens, choosing the divisor by the caller-supplied classification.
pub fn estimate_tokens_with(text: &str, is_code_or_json: bool) -> u64 {
    let chars = text.chars().count() as f64;
    let divisor = if is_code_or_json {
        CODE_DIVISOR
    } else {
        PROSE_DIVISOR
    };
    (chars / divisor).ceil() as u64
}

/// Cheap, allocation-free classifier. We treat text as code/JSON when the first
/// non-whitespace character opens a JSON value (`{` or `[`). This intentionally
/// stays simple: the heuristic only needs to catch the common, high-volume case
/// of structured tool output (large JSON blobs), where the tighter divisor
/// matters. Everything else is treated as prose.
pub fn looks_like_code_or_json(text: &str) -> bool {
    matches!(text.trim_start().as_bytes().first(), Some(b'{') | Some(b'['))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_uses_four_char_divisor() {
        // 8 chars of prose -> ceil(8/4) = 2
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn json_uses_tighter_divisor() {
        let json = r#"{"a":1}"#; // 7 chars, starts with '{'
        assert!(looks_like_code_or_json(json));
        // ceil(7 / 3.6) = ceil(1.94) = 2
        assert_eq!(estimate_tokens(json), 2);
    }

    #[test]
    fn leading_whitespace_before_brace_still_detected() {
        assert!(looks_like_code_or_json("   \n  [1,2,3]"));
    }

    #[test]
    fn plain_text_is_not_code() {
        assert!(!looks_like_code_or_json("hello world"));
    }

    #[test]
    fn empty_string_is_zero_tokens() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn counts_unicode_scalar_values_not_bytes() {
        // "café" = 4 chars -> ceil(4/4) = 1, even though 'é' is 2 UTF-8 bytes.
        assert_eq!(estimate_tokens("café"), 1);
    }
}
