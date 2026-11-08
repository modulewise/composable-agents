//! The token usage of model exchanges, by provider and model.

use crate::types::{ModelExchange, TokenUsage};

/// The token usage of the calls to one provider and requested model.
#[derive(Debug)]
pub struct ModelUsage {
    /// The provider the calls were sent to.
    pub provider: String,
    /// The model the calls requested.
    pub model: String,
    /// The number of calls.
    pub calls: u32,
    /// The calls that failed or were canceled, whose counts may be partial.
    pub incomplete_calls: u32,
    /// Each count summed over the calls, or `none` if no call reported it.
    pub usage: TokenUsage,
}

/// The token usage of `exchanges`, per provider and requested model, in the
/// order each first appears. Does not include any exchange that was not sent
/// as a call.
pub fn usage_by_model(exchanges: &[ModelExchange]) -> Vec<ModelUsage> {
    let mut totals: Vec<ModelUsage> = Vec::new();
    for exchange in exchanges {
        let (Some(provider), Some(model)) = (&exchange.provider, &exchange.request_model) else {
            continue;
        };
        let index = match totals
            .iter()
            .position(|total| &total.provider == provider && &total.model == model)
        {
            Some(index) => index,
            None => {
                totals.push(ModelUsage {
                    provider: provider.clone(),
                    model: model.clone(),
                    calls: 0,
                    incomplete_calls: 0,
                    usage: TokenUsage::default(),
                });
                totals.len() - 1
            }
        };
        let total = &mut totals[index];
        total.calls += 1;
        if exchange.output.is_none() {
            total.incomplete_calls += 1;
        }
        total.usage = add_usage(&total.usage, &exchange.usage);
    }
    totals
}

// Every count `None`: no counts reported. The struct is generated, and
// wasmtime's `bindgen!` can't add a derive to one type.
#[allow(clippy::derivable_impls)]
impl Default for TokenUsage {
    fn default() -> Self {
        TokenUsage {
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        }
    }
}

// Each count summed, or `None` if neither reported it.
fn add_usage(total: &TokenUsage, usage: &TokenUsage) -> TokenUsage {
    let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    };
    TokenUsage {
        input_tokens: sum(total.input_tokens, usage.input_tokens),
        output_tokens: sum(total.output_tokens, usage.output_tokens),
        cache_read_tokens: sum(total.cache_read_tokens, usage.cache_read_tokens),
        cache_write_tokens: sum(total.cache_write_tokens, usage.cache_write_tokens),
        reasoning_tokens: sum(total.reasoning_tokens, usage.reasoning_tokens),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelOutputItem, ModelRequest};

    fn exchange(
        provider: Option<&str>,
        model: Option<&str>,
        completed: bool,
        usage: TokenUsage,
    ) -> ModelExchange {
        ModelExchange {
            request: ModelRequest {
                input: Vec::new(),
                max_tokens: None,
            },
            output: completed.then(|| vec![ModelOutputItem::Text("Hi".to_string())]),
            opaque_content: None,
            provider: provider.map(str::to_string),
            request_model: model.map(str::to_string),
            response_model: None,
            usage,
        }
    }

    fn counts(input: Option<u64>, output: Option<u64>) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..TokenUsage::default()
        }
    }

    #[test]
    fn calls_are_summed_per_provider_and_model() {
        let usage = usage_by_model(&[
            exchange(Some("openai"), Some("gpt"), true, counts(Some(10), Some(5))),
            exchange(
                Some("anthropic"),
                Some("claude"),
                true,
                counts(Some(7), Some(1)),
            ),
            exchange(Some("openai"), Some("gpt"), true, counts(Some(20), Some(7))),
        ]);
        let summary: Vec<_> = usage
            .iter()
            .map(|total| {
                (
                    total.provider.as_str(),
                    total.model.as_str(),
                    total.calls,
                    total.usage.input_tokens,
                    total.usage.output_tokens,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("openai", "gpt", 2, Some(30), Some(12)),
                ("anthropic", "claude", 1, Some(7), Some(1)),
            ]
        );
    }

    #[test]
    fn an_incomplete_call_is_counted_with_the_counts_it_reported() {
        let usage = usage_by_model(&[
            exchange(
                Some("anthropic"),
                Some("claude"),
                true,
                counts(Some(10), Some(5)),
            ),
            // Canceled after the provider reported its input count.
            exchange(
                Some("anthropic"),
                Some("claude"),
                false,
                counts(Some(12), None),
            ),
        ]);
        assert_eq!(usage[0].calls, 2);
        assert_eq!(usage[0].incomplete_calls, 1);
        assert_eq!(usage[0].usage.input_tokens, Some(22));
        assert_eq!(usage[0].usage.output_tokens, Some(5));
    }

    #[test]
    fn a_count_no_call_reported_remains_none() {
        let usage = usage_by_model(&[exchange(
            Some("ollama"),
            Some("gemma"),
            true,
            counts(Some(10), Some(5)),
        )]);
        assert_eq!(usage[0].usage.cache_read_tokens, None);
    }

    #[test]
    fn an_exchange_not_sent_as_a_call_is_not_included() {
        let usage = usage_by_model(&[exchange(None, None, false, TokenUsage::default())]);
        assert!(usage.is_empty());
    }
}
