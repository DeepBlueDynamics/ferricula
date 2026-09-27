use std::env;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::Value;

use crate::model::{InferenceTransport, ProviderRequest, ProviderResponse};

/// Production HTTP transport. It resolves secret values only at execution
/// time from the env-var names carried in a validated provider request.
#[derive(Debug, Default)]
pub struct HttpInferenceTransport;

impl HttpInferenceTransport {
    fn client(timeout_ms: u64) -> Result<Client> {
        Ok(Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()?)
    }

    fn optional_key(name: &str) -> Result<Option<String>> {
        if name.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(env::var(name).with_context(|| {
            format!("model credential environment variable {name} is not set")
        })?))
    }
}

impl InferenceTransport for HttpInferenceTransport {
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        match request {
            ProviderRequest::NoModel { reason, echo } => Ok(ProviderResponse {
                text: format!("[no-model:{reason}] {echo}"),
                input_tokens: 0,
                output_tokens: 0,
                raw: serde_json::json!({ "mode": "no_model", "reason": reason }),
            }),
            ProviderRequest::OpenAiCompatible {
                url,
                api_key_env,
                timeout_ms,
                body,
            } => {
                let client = Self::client(*timeout_ms)?;
                let mut request = client.post(url).json(body);
                if let Some(key) = Self::optional_key(api_key_env)? {
                    request = request.bearer_auth(key);
                }
                let raw: Value = request.send()?.error_for_status()?.json()?;
                let text = raw
                    .pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
                    .context("OpenAI-compatible response contained no message content")?
                    .to_string();
                Ok(ProviderResponse {
                    text,
                    input_tokens: usage_u32(&raw, "/usage/prompt_tokens"),
                    output_tokens: usage_u32(&raw, "/usage/completion_tokens"),
                    raw,
                })
            }
            ProviderRequest::Anthropic {
                url,
                api_key_env,
                timeout_ms,
                body,
            } => {
                let key = Self::optional_key(api_key_env)?
                    .context("Anthropic request requires an API key environment variable")?;
                let raw: Value = Self::client(*timeout_ms)?
                    .post(url)
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01")
                    .json(body)
                    .send()?
                    .error_for_status()?
                    .json()?;
                let Some(content) = raw.get("content").and_then(Value::as_array) else {
                    bail!("Anthropic response contained no content array");
                };
                let text = content
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    bail!("Anthropic response contained no text content");
                }
                Ok(ProviderResponse {
                    text,
                    input_tokens: usage_u32(&raw, "/usage/input_tokens"),
                    output_tokens: usage_u32(&raw, "/usage/output_tokens"),
                    raw,
                })
            }
        }
    }
}

fn usage_u32(value: &Value, pointer: &str) -> u32 {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_usage_is_zero() {
        assert_eq!(usage_u32(&serde_json::json!({}), "/usage/input_tokens"), 0);
    }
}
