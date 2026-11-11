use super::LanguageModel;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use std::io;
use std::time::Duration;

const ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";

pub struct Groq {
    client: Client,
    model: String,
}

impl Groq {
    pub fn new(key: &str, model: String) -> io::Result<Self> {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "GROQ_KEY contains invalid header characters",
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

        Ok(Self { client, model })
    }
}

impl LanguageModel for Groq {
    fn generate(&self, instructions: &str, input: &str) -> io::Result<String> {
        let mut body = serde_json::json!({
            "model": self.model,
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
            "max_completion_tokens": 4096
        });

        match self.model.as_str() {
            "openai/gpt-oss-120b" | "openai/gpt-oss-20b" => {
                body["reasoning_effort"] = serde_json::json!("medium");
                body["include_reasoning"] = serde_json::json!(false);
            }
            "qwen/qwen3.8-27b" => {
                body["reasoning_effort"] = serde_json::json!("medium");
                body["reasoning_format"] = serde_json::json!("hidden");
            }
            _ => {}
        }

        let response = self
            .client
            .post(ENDPOINT)
            .json(&body)
            .send()
            .map_err(request_error)?;

        let status = response.status();

        if !status.is_success() {
            let explanation = match status.as_u16() {
                401 => "authentication failed; check GROQ_KEY",
                403 => "access denied; check project and model permissions",
                404 => "endpoint or model unavailable; check GROQ_MODEL",
                429 => "rate limit or quota exceeded; try again later",
                400 | 422 => "request rejected; check model and parameter support",
                500..=599 => "Groq service error; try again later",
                _ => "request failed",
            };

            return Err(io::Error::other(format!(
                "Groq HTTP {}: {explanation}",
                status.as_u16()
            )));
        }

        let bytes = response.bytes().map_err(request_error)?;
        parse_response(&bytes)
    }
}

fn request_error(error: reqwest::Error) -> io::Error {
    if error.is_timeout() {
        io::Error::new(io::ErrorKind::TimedOut, "Groq request timed out")
    } else {
        io::Error::other(format!("Groq request failed: {error}"))
    }
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
    usage: Option<Usage>,
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
        .map_err(|_| invalid_response("Groq returned an invalid response"))?;

    let choice = result
        .choices
        .first()
        .ok_or_else(|| invalid_response("Groq returned no answer choices"))?;

    match choice.finish_reason.as_deref() {
        Some("stop") => {}
        Some("length") => {
            return Err(invalid_response(
                "Groq answer was truncated by the token limit",
            ));
        }
        _ => {
            return Err(invalid_response(
                "Groq did not finish with a normal text answer",
            ));
        }
    }

    let answer = choice
        .message
        .content
        .as_deref()
        .map(str::trim)
        .filter(|answer| !answer.is_empty())
        .ok_or_else(|| invalid_response("Groq returned an empty answer"))?;

    if let Some(usage) = result.usage {
        eprintln!(
            "Groq tokens: {} input, {} output, {} total",
            usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
        );
    }

    Ok(answer.to_string())
}

fn invalid_response(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
