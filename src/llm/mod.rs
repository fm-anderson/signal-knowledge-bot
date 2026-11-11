mod agy;
mod groq;
mod settings;

#[cfg(test)]
mod tests;

use std::io;
use std::path::Path;

pub use settings::load_model;

const NO_EVIDENCE: &str = "I cannot find the answer in the supplied documents.";

pub trait LanguageModel {
    fn generate(&self, instructions: &str, input: &str) -> io::Result<String>;
}

pub struct Source<'a> {
    pub document: &'a str,
    pub page: usize,
    pub text: &'a str,
}

pub fn answer_question<'a>(
    model: &(impl LanguageModel + ?Sized),
    question: &str,
    sources: impl IntoIterator<Item = Source<'a>>,
    instructions: &str,
) -> io::Result<String> {
    if question.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The question cannot be empty",
        ));
    }

    if instructions.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Answering instructions cannot be empty",
        ));
    }

    let sources: Vec<_> = sources.into_iter().collect();

    if sources.is_empty() {
        return Ok(NO_EVIDENCE.to_string());
    }

    let blocks: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            serde_json::json!({
                "id": format!("S{}", index + 1),
                "document": document_name(source.document),
                "pdf_page": source.page,
                "text": source.text
            })
        })
        .collect();

    // JSON keeps metadata and page text in separate fields and escapes
    // quotes/newlines without changing the underlying source text.
    let input = serde_json::to_string_pretty(&serde_json::json!({
        "user_request": question,
        "sources": blocks
    }))
    .map_err(io::Error::other)?;

    let answer = model.generate(instructions, &input)?;
    let rendered = render_citations(&answer, &sources);

    #[cfg(test)]
    if let Err(error) = &rendered {
        eprintln!(
            "\nCitation validation failed: {error}\n\
             --- Raw model response ---\n\
             {answer}\n\
             --- End raw response ---"
        );
    }

    rendered
}

fn document_name(document: &str) -> &str {
    Path::new(document)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(document)
}

fn render_citations(answer: &str, sources: &[Source<'_>]) -> io::Result<String> {
    let normalized = answer.replace('【', "[").replace('】', "]");
    let answer = normalized.trim();

    if answer == NO_EVIDENCE {
        return Ok(answer.to_string());
    }

    let mut rendered = String::new();
    let mut remaining = answer;
    let mut citation_count = 0;

    while let Some(start) = remaining.find("[S") {
        rendered.push_str(&remaining[..start]);
        remaining = &remaining[start..];

        let end = remaining
            .find(']')
            .ok_or_else(|| invalid_citation("Unclosed source citation"))?;

        let id = &remaining[2..end];

        if id.is_empty() || id.starts_with('0') || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_citation(
                "Malformed source citation; expected [S1], [S2], etc.",
            ));
        }

        let number = id
            .parse::<usize>()
            .map_err(|_| invalid_citation("Invalid source citation number"))?;

        let source = number
            .checked_sub(1)
            .and_then(|index| sources.get(index))
            .ok_or_else(|| invalid_citation("Answer cited an unknown source ID"))?;

        rendered.push_str(&format!(
            "({}, PDF page {})",
            document_name(source.document),
            source.page
        ));

        citation_count += 1;
        remaining = &remaining[end + 1..];
    }

    rendered.push_str(remaining);

    if citation_count == 0 {
        return Err(invalid_citation(
            "Answer did not contain any valid source citations",
        ));
    }

    Ok(rendered)
}

fn invalid_citation(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn load_instructions(path: &Path) -> io::Result<String> {
    let instructions = std::fs::read_to_string(path)?;

    if instructions.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Answering instructions are empty: {}", path.display()),
        ));
    }

    Ok(instructions)
}
