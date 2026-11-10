use serde::Deserialize;
use std::io;
use std::process::{Command, Stdio};

pub trait LanguageModel {
    fn generate(&self, prompt: &str) -> io::Result<String>;
}

pub struct Agy;

impl LanguageModel for Agy {
    fn generate(&self, prompt: &str) -> io::Result<String> {
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
                prompt,
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

fn parse_response(output: &[u8]) -> io::Result<String> {
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

pub fn answer_question(
    model: &impl LanguageModel,
    question: &str,
    context: &str,
    instructions: &str,
) -> io::Result<String> {
    if question.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The question cannot be empty",
        ));
    }

    if context.trim().is_empty() {
        return Ok(
            "No matching document pages were available to answer this question.".to_string(),
        );
    }

    let prompt = format!(
        "{instructions}\n\
\n\
<user_request>\n\
{question}\n\
</user_request>\n\
\n\
<sources>\n\
{context}\n\
</sources>"
    );

    model.generate(&prompt)
}

pub fn load_instructions(path: &std::path::Path) -> io::Result<String> {
    let instructions = std::fs::read_to_string(path)?;

    if instructions.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Answering instructions are empty: {}", path.display()),
        ));
    }

    Ok(instructions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct MockModel {
        prompts: RefCell<Vec<String>>,
    }

    impl LanguageModel for MockModel {
        fn generate(&self, prompt: &str) -> io::Result<String> {
            self.prompts.borrow_mut().push(prompt.to_string());
            Ok("Mock answer with a citation.".to_string())
        }
    }

    #[test]
    fn parses_success_and_rejects_invalid_responses() {
        let answer =
            parse_response(br#"{"status":"SUCCESS","response":"  Supported answer. \n"}"#).unwrap();

        assert_eq!(answer, "Supported answer.");

        for invalid in [
            &b"not json"[..],
            &br#"{"status":"FAILED","response":"An apparent answer"}"#[..],
            &br#"{"status":"SUCCESS","response":"  "}"#[..],
            &br#"{"status":"SUCCESS"}"#[..],
        ] {
            assert!(parse_response(invalid).is_err());
        }
    }

    #[test]
    fn sends_question_and_sources_but_skips_empty_context() {
        let model = MockModel::default();
        let question = "What does this document cover?";
        let context = "[Document: policy.pdf | PDF page 3]\nSource text.";

        let answer =
            answer_question(&model, question, context, "Use only the supplied sources.").unwrap();

        assert_eq!(answer, "Mock answer with a citation.");
        assert_eq!(model.prompts.borrow().len(), 1);

        {
            let prompts = model.prompts.borrow();
            assert!(prompts[0].contains(question));
            assert!(prompts[0].contains(context));
        }

        let no_context =
            answer_question(&model, question, " \n", "Use only the supplied sources.").unwrap();

        assert!(no_context.contains("No matching document pages"));
        assert_eq!(model.prompts.borrow().len(), 1);

        assert!(answer_question(&model, " ", context, "Use only the supplied sources.",).is_err());
        assert_eq!(model.prompts.borrow().len(), 1);
        assert!(model.prompts.borrow()[0].contains("Use only the supplied sources."));
    }

    #[test]
    fn propagates_model_failure() {
        struct FailingModel;

        impl LanguageModel for FailingModel {
            fn generate(&self, _: &str) -> io::Result<String> {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Model request timed out",
                ))
            }
        }

        let error = answer_question(
            &FailingModel,
            "A question",
            "Some source text",
            "Use only the supplied sources.",
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
