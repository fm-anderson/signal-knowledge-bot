use super::LanguageModel;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, RETRY_AFTER};
use serde::Deserialize;
use std::io;
use std::time::Duration;

const ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

pub struct OpenRouter {
    client: Client,
    model: String,
    key: String,
}

impl OpenRouter {
    pub fn new(key: &str, model: String) -> io::Result<Self> {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "OPENROUTER_KEY contains invalid header characters",
            )
        })?;

        authorization.set_sensitive(true);

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);

        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(request_error)?;

        Ok(Self {
            client,
            model,
            key: key.to_string(),
        })
    }
}

impl LanguageModel for OpenRouter {
    fn generate(&self, instructions: &str, input: &str) -> io::Result<String> {
        let body = build_request(&self.model, instructions, input);

        let response = self
            .client
            .post(ENDPOINT)
            .json(&body)
            .send()
            .map_err(request_error)?;

        let status = response.status();

        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        if !status.is_success() {
            let bytes = response.bytes().map_err(request_error)?;

            return Err(http_error(
                status.as_u16(),
                retry_after.as_deref(),
                &bytes,
                &self.key,
            ));
        }

        let bytes = response.bytes().map_err(request_error)?;
        parse_response(&bytes)
    }
}

pub(super) fn build_request(model: &str, instructions: &str, input: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": instructions
            },
            {
                "role": "user",
                "content": input
            }
        ],
        "stream": false,
        "max_tokens": 4096
    })
}

pub(super) fn http_error(
    status: u16,
    retry_after: Option<&str>,
    bytes: &[u8],
    key: &str,
) -> io::Error {
    let explanation = match status {
        400 | 422 => "request rejected",
        401 => "authentication failed; check OPENROUTER_KEY",
        402 => "account credit or spending limit prevents this request",
        403 => "access denied",
        404 => "model or endpoint unavailable; check OPENROUTER_MODEL",
        408 => "request timed out",
        429 => "rate limit or quota exceeded",
        500..=599 => "provider unavailable or service error",
        _ => "request failed",
    };

    let mut details = vec![format!("OpenRouter HTTP {status}: {explanation}")];

    if let Ok(body) = serde_json::from_slice::<serde_json::Value>(bytes) {
        let error = &body["error"];

        if let Some(code) = error.get("code") {
            let code = match code {
                serde_json::Value::String(value) => Some(value.clone()),
                serde_json::Value::Number(value) => Some(value.to_string()),
                _ => None,
            };

            if let Some(code) = code {
                details.push(format!("API code: {}", diagnostic_text(&code, key)));
            }
        }

        if let Some(message) = error["message"].as_str() {
            details.push(format!("Message: {}", diagnostic_text(message, key)));
        }

        if let Some(provider) = error["metadata"]["provider_name"].as_str() {
            details.push(format!("Provider: {}", diagnostic_text(provider, key)));
        }
    }

    if let Some(value) = retry_after {
        details.push(format!("Retry-After: {}", diagnostic_text(value, key)));
    }

    io::Error::other(details.join(" | "))
}

fn diagnostic_text(text: &str, key: &str) -> String {
    let redacted = if key.is_empty() {
        text.to_string()
    } else {
        text.replace(key, "[REDACTED]")
    };

    let single_line = redacted.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut shortened: String = single_line
        .chars()
        .filter(|character| !character.is_control())
        .take(500)
        .collect();

    if single_line.chars().count() > 500 {
        shortened.push('…');
    }

    shortened
}

fn request_error(error: reqwest::Error) -> io::Error {
    if error.is_timeout() {
        io::Error::new(io::ErrorKind::TimedOut, "OpenRouter request timed out")
    } else {
        io::Error::other(format!("OpenRouter request failed: {error}"))
    }
}

#[derive(Deserialize)]
struct Completion {
    #[serde(default)]
    choices: Vec<Choice>,
    error: Option<serde_json::Value>,
    usage: Option<Usage>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

pub(super) fn parse_response(bytes: &[u8]) -> io::Result<String> {
    let result: Completion = serde_json::from_slice(bytes)
        .map_err(|_| invalid_response("OpenRouter returned an invalid response"))?;

    if result.error.is_some() {
        return Err(io::Error::other(
            "OpenRouter returned a provider error instead of an answer",
        ));
    }

    let choice = result
        .choices
        .first()
        .ok_or_else(|| invalid_response("OpenRouter returned no answer choices"))?;

    match choice.finish_reason.as_deref() {
        Some("stop") => {}
        Some("length") => {
            return Err(invalid_response(
                "OpenRouter answer was truncated by the token limit",
            ));
        }
        _ => {
            return Err(invalid_response(
                "OpenRouter did not finish with a normal text answer",
            ));
        }
    }

    let answer = choice
        .message
        .content
        .as_deref()
        .map(str::trim)
        .filter(|answer| !answer.is_empty())
        .ok_or_else(|| invalid_response("OpenRouter returned an empty answer"))?;

    if let Some(model) = result.model {
        eprintln!("OpenRouter response model: {model}");
    }

    if let Some(usage) = result.usage {
        eprintln!(
            "OpenRouter tokens: {} input, {} output, {} total",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }

    Ok(answer.to_string())
}

fn invalid_response(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
