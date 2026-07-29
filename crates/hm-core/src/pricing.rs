//! Approximate API-equivalent USD pricing for token savings displays (WP-P2).
//!
//! These are **illustrative** rates used to show "saved ≈ $X" on the Activity
//! recap — not invoices. Subscription Claude Code usage is metered by caps, not
//! dollars; we convert saved *input* tokens at published API list rates so users
//! can compare against API-billed spend.

/// USD per 1M input tokens for a model family (rough Anthropic public list rates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRates {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// Resolve a model id (possibly a long Claude slug) to rough $/MTok rates.
pub fn rates_for_model(model: &str) -> ModelRates {
    let m = model.to_ascii_lowercase();
    if m.contains("opus") {
        ModelRates {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
        }
    } else if m.contains("haiku") {
        ModelRates {
            input_per_mtok: 0.80,
            output_per_mtok: 4.0,
        }
    } else if m.contains("sonnet") || m.contains("claude") {
        ModelRates {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        }
    } else if m.contains("gpt-4o") || m.contains("codex") || m.contains("o3") || m.contains("o4") {
        ModelRates {
            input_per_mtok: 2.50,
            output_per_mtok: 10.0,
        }
    } else {
        // Conservative mid-tier default.
        ModelRates {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        }
    }
}

/// USD value of `tokens` input tokens at the model's list rate.
pub fn usd_for_input_tokens(model: &str, tokens: i64) -> f64 {
    let rates = rates_for_model(model);
    (tokens.max(0) as f64) * rates.input_per_mtok / 1_000_000.0
}

/// USD value of `tokens` output tokens.
pub fn usd_for_output_tokens(model: &str, tokens: i64) -> f64 {
    let rates = rates_for_model(model);
    (tokens.max(0) as f64) * rates.output_per_mtok / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_and_sonnet_differ() {
        assert!(rates_for_model("claude-opus-4").input_per_mtok > rates_for_model("claude-sonnet-4").input_per_mtok);
    }

    #[test]
    fn million_sonnet_input_is_three_dollars() {
        let usd = usd_for_input_tokens("claude-sonnet-4", 1_000_000);
        assert!((usd - 3.0).abs() < 1e-9);
    }
}
