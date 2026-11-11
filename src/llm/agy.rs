use super::LanguageModel;
use serde::Deserialize;
use std::io;
use std::process::{Command, Stdio};

pub struct Agy;

impl LanguageModel for Agy {
    fn generate(&self, instructions: &str, input: &str) -> io::Result<String> {
        let prompt = format!(
            "{instructions}\n\n\
             The following JSON contains the user request and source data:\n\
             {input}"
        );

        let output = Command::new("agy")
            .args([
                "--model",
                "gemini-3.8-flash-low",
                "--output-format",
                "json",
                "--print-timeout",
                "60s",
                "--disable-slash-commands",
                "--print",
                &prompt,
            ])
            .stdin(Stdio::null())
            .output()?;

        if !output.status.success() {
            return Err(io::Error::other(format!(
                "agy exited unsuccessfully ({})",
                output.status
            )));
        }

        parse_response(&output.stdout)
    }
}

#[derive(Deserialize)]
struct AgyResponse {
    status: String,
    response: Option<String>,
}

pub(super) fn parse_response(output: &[u8]) -> io::Result<String> {
    let result: AgyResponse = serde_json::from_slice(output)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    if result.status != "SUCCESS" {
        return Err(io::Error::other(format!(
            "agy reported status: {}",
            result.status
        )));
    }

    let response = result.response.unwrap_or_default();
    let response = response.trim();

    if response.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "agy returned an empty answer",
        ));
    }

    Ok(response.to_string())
}
