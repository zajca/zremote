use super::transcript::TokenUsageData;

/// Aggregated metrics for a loop, ready to send as a protocol message.
#[derive(Debug, Clone)]
pub struct AggregatedMetrics {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub model: String,
    pub context_used: u64,
    pub context_max: u64,
    pub estimated_cost_usd: f64,
}

/// Model pricing per million tokens (USD).
struct ModelPricing {
    input_per_m: f64,
    output_per_m: f64,
    cache_read_per_m: f64,
}

/// Get pricing for a known model. Returns None for unknown models.
fn get_pricing(model: &str) -> Option<ModelPricing> {
    // Normalize model name - strip date suffixes like "-20250514"
    let base = model
        .trim_end_matches(|c: char| c == '-' || c.is_ascii_digit())
        .trim_end_matches('-');

    match base {
        // Claude Sonnet 4 / 4.5
        "claude-sonnet-4" | "claude-sonnet" | "claude-4-sonnet" => Some(ModelPricing {
            input_per_m: 3.0,
            output_per_m: 15.0,
            cache_read_per_m: 0.30,
        }),
        // Claude Opus 4 / 4.5 / 4.6
        "claude-opus-4" | "claude-opus" | "claude-4-opus" | "claude-opus-4-6" => {
            Some(ModelPricing {
                input_per_m: 15.0,
                output_per_m: 75.0,
                cache_read_per_m: 1.50,
            })
        }
        // Claude Haiku 4.5
        "claude-haiku-4" | "claude-haiku" | "claude-4-haiku" | "claude-haiku-4-5" => {
            Some(ModelPricing {
                input_per_m: 0.80,
                output_per_m: 4.0,
                cache_read_per_m: 0.08,
            })
        }
        _ => None,
    }
}

/// Calculate cost based on token usage and model.
fn calculate_cost(data: &TokenUsageData) -> f64 {
    let model = data.model.as_deref().unwrap_or("");
    let Some(pricing) = get_pricing(model) else {
        return 0.0;
    };

    let input_cost = data.input_tokens as f64 * pricing.input_per_m / 1_000_000.0;
    let output_cost = data.output_tokens as f64 * pricing.output_per_m / 1_000_000.0;
    let cache_cost = data.cache_read_input_tokens as f64 * pricing.cache_read_per_m / 1_000_000.0;

    input_cost + output_cost + cache_cost
}

/// Aggregate token usage data into a single metrics struct.
///
/// Returns None if there's no data to aggregate.
pub fn aggregate_metrics(token_data: &[TokenUsageData]) -> Option<AggregatedMetrics> {
    if token_data.is_empty() {
        return None;
    }

    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut model = String::new();

    for data in token_data {
        total_in += data.input_tokens + data.cache_read_input_tokens
            + data.cache_creation_input_tokens;
        total_out += data.output_tokens;
        total_cost += calculate_cost(data);

        if model.is_empty() && let Some(ref m) = data.model {
            model.clone_from(m);
        }
    }

    // Estimate context usage from the last entry's input tokens
    let context_used = token_data
        .last()
        .map(|d| d.input_tokens + d.cache_read_input_tokens)
        .unwrap_or(0);

    // Context max depends on model
    let context_max = match model.as_str() {
        m if m.contains("opus") => 1_000_000,
        m if m.contains("sonnet") => 200_000,
        m if m.contains("haiku") => 200_000,
        _ => 200_000,
    };

    Some(AggregatedMetrics {
        tokens_in: total_in,
        tokens_out: total_out,
        model,
        context_used,
        context_max,
        estimated_cost_usd: total_cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_empty_returns_none() {
        assert!(aggregate_metrics(&[]).is_none());
    }

    #[test]
    fn aggregate_single_entry() {
        let data = vec![TokenUsageData {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_input_tokens: 100,
            cache_creation_input_tokens: 50,
            model: Some("claude-sonnet-4-20250514".to_string()),
        }];

        let metrics = aggregate_metrics(&data).unwrap();
        assert_eq!(metrics.tokens_in, 1150); // 1000 + 100 + 50
        assert_eq!(metrics.tokens_out, 500);
        assert!(metrics.model.contains("sonnet"));
        assert!(metrics.estimated_cost_usd > 0.0);
    }

    #[test]
    fn aggregate_multiple_entries() {
        let data = vec![
            TokenUsageData {
                input_tokens: 1000,
                output_tokens: 500,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                model: Some("claude-sonnet-4-20250514".to_string()),
            },
            TokenUsageData {
                input_tokens: 2000,
                output_tokens: 800,
                cache_read_input_tokens: 500,
                cache_creation_input_tokens: 0,
                model: Some("claude-sonnet-4-20250514".to_string()),
            },
        ];

        let metrics = aggregate_metrics(&data).unwrap();
        assert_eq!(metrics.tokens_in, 3500); // 1000 + 2000 + 500 (cache_read)
        assert_eq!(metrics.tokens_out, 1300); // 500 + 800
    }

    #[test]
    fn cost_calculation_sonnet() {
        let data = TokenUsageData {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: Some("claude-sonnet-4-20250514".to_string()),
        };

        let cost = calculate_cost(&data);
        // $3/M input + $15/M output = $18
        assert!((cost - 18.0).abs() < 0.01);
    }

    #[test]
    fn cost_calculation_opus() {
        let data = TokenUsageData {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: Some("claude-opus-4-20250514".to_string()),
        };

        let cost = calculate_cost(&data);
        // $15/M input + $75/M output = $90
        assert!((cost - 90.0).abs() < 0.01);
    }

    #[test]
    fn cost_calculation_with_cache() {
        let data = TokenUsageData {
            input_tokens: 100_000,
            output_tokens: 50_000,
            cache_read_input_tokens: 500_000,
            cache_creation_input_tokens: 0,
            model: Some("claude-sonnet-4-20250514".to_string()),
        };

        let cost = calculate_cost(&data);
        // input: 100k * $3/M = $0.30
        // output: 50k * $15/M = $0.75
        // cache_read: 500k * $0.30/M = $0.15
        // total = $1.20
        assert!((cost - 1.20).abs() < 0.01);
    }

    #[test]
    fn cost_unknown_model_returns_zero() {
        let data = TokenUsageData {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: Some("unknown-model-v1".to_string()),
        };

        assert_eq!(calculate_cost(&data), 0.0);
    }

    #[test]
    fn cost_no_model_returns_zero() {
        let data = TokenUsageData {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: None,
        };

        assert_eq!(calculate_cost(&data), 0.0);
    }

    #[test]
    fn context_max_by_model() {
        let opus = vec![TokenUsageData {
            model: Some("claude-opus-4-20250514".to_string()),
            ..Default::default()
        }];
        assert_eq!(aggregate_metrics(&opus).unwrap().context_max, 1_000_000);

        let sonnet = vec![TokenUsageData {
            model: Some("claude-sonnet-4-20250514".to_string()),
            ..Default::default()
        }];
        assert_eq!(aggregate_metrics(&sonnet).unwrap().context_max, 200_000);
    }
}
