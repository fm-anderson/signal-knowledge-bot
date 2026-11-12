use super::agy::parse_response;
use super::groq::parse_response as parse_groq_response;
use super::*;
use std::cell::RefCell;

#[derive(Default)]
struct MockModel {
    requests: RefCell<Vec<(String, String)>>,
}

impl LanguageModel for MockModel {
    fn generate(&self, instructions: &str, input: &str) -> io::Result<String> {
        self.requests
            .borrow_mut()
            .push((instructions.to_string(), input.to_string()));

        Ok("Supported answer [S1].".to_string())
    }
}

fn sources() -> Vec<Source<'static>> {
    vec![
        Source {
            document: "manuals/policy.pdf",
            page: 3,
            text: "First document's text, including \"quotes\".\nSecond line.",
        },
        Source {
            document: "manuals/guide.pdf",
            page: 8,
            text: "A separate document with different conditions.",
        },
    ]
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
fn separates_instructions_and_preserves_source_identity_and_text() {
    let model = MockModel::default();
    let question = "What does this document cover?";
    let instructions = "Use only the supplied sources.";

    let answer = answer_question(&model, question, sources(), instructions).unwrap();

    assert_eq!(answer, "Supported answer (policy.pdf, PDF page 3).");

    let requests = model.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, instructions);

    let input: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();

    assert_eq!(input["user_request"], question);
    assert_eq!(input["sources"].as_array().unwrap().len(), 2);

    for (index, source) in sources().iter().enumerate() {
        let block = &input["sources"][index];
        assert_eq!(block["id"], format!("S{}", index + 1));
        assert_eq!(block["document"], document_name(source.document));
        assert_eq!(block["pdf_page"], source.page);
        assert_eq!(block["text"], source.text);
    }
}

#[test]
fn skips_empty_sources_and_rejects_invalid_inputs() {
    let model = MockModel::default();
    let instructions = "Use only the supplied sources.";

    let answer =
        answer_question(&model, "A question", Vec::<Source<'_>>::new(), instructions).unwrap();

    assert_eq!(answer, NO_EVIDENCE);
    assert!(answer_question(&model, " ", sources(), instructions).is_err());
    assert!(answer_question(&model, "A question", sources(), " ").is_err());
    assert!(model.requests.borrow().is_empty());
}

#[test]
fn citations_render_in_answer_order_and_invalid_ids_fail() {
    let sources = sources();

    let answer = render_citations(
        "First claim [S2]. Another claim [S1]. Repeated [S2].",
        &sources,
    )
    .unwrap();

    assert_eq!(
        answer,
        "First claim (guide.pdf, PDF page 8). \
         Another claim (policy.pdf, PDF page 3). \
         Repeated (guide.pdf, PDF page 8)."
    );

    for invalid in [
        "Claim [S0].",
        "Claim [S3].",
        "Claim [S01].",
        "Claim [S1, S2].",
        "Claim [S1",
        "Claim [S999999999999999999999999999999].",
        "Claim without a citation.",
        "",
    ] {
        assert!(render_citations(invalid, &sources).is_err());
    }

    assert_eq!(
        render_citations(NO_EVIDENCE, &sources).unwrap(),
        NO_EVIDENCE
    );

    assert_eq!(
        render_citations("Claim【S1】.", &sources).unwrap(),
        "Claim(policy.pdf, PDF page 3)."
    );

    assert!(render_citations("Claim【S99】.", &sources).is_err());
}

#[test]
fn propagates_model_failure() {
    struct FailingModel;

    impl LanguageModel for FailingModel {
        fn generate(&self, _: &str, _: &str) -> io::Result<String> {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Model request timed out",
            ))
        }
    }

    let error = answer_question(
        &FailingModel,
        "A question",
        sources(),
        "Use only the supplied sources.",
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn groq_accepts_complete_answers_and_rejects_invalid_results() {
    let valid = br#"{
        "choices": [{
            "message": {"content": "  Supported answer.  "},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120
        }
    }"#;

    assert_eq!(parse_groq_response(valid).unwrap(), "Supported answer.");

    for invalid in [
        &b"not json"[..],
        &br#"{"choices":[]}"#[..],
        &br#"{
            "choices":[{
                "message":{"content":"Partial answer"},
                "finish_reason":"length"
            }]
        }"#[..],
        &br#"{
            "choices":[{
                "message":{"content":" "},
                "finish_reason":"stop"
            }]
        }"#[..],
        &br#"{
            "choices":[{
                "message":{"content":null},
                "finish_reason":"tool_calls"
            }]
        }"#[..],
    ] {
        assert!(parse_groq_response(invalid).is_err());
    }
}

#[test]
#[ignore = "Manual diagnostic: calls the configured model using local PDF pages"]
fn diagnostic_complete_licensing_section() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    let connection = rusqlite::Connection::open_with_flags(
        root.join("knowledge.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();

    let mut statement = connection
        .prepare(
            "SELECT documents.path, pages.page_number, pages.text
             FROM pages
             JOIN documents ON documents.id = pages.document_id
             WHERE (
                 documents.path = 'manuals/dafi24-301.pdf'
                 OR documents.path LIKE '%/dafi24-301.pdf'
             )
             AND pages.page_number BETWEEN 61 AND 66
             ORDER BY pages.page_number",
        )
        .unwrap();

    let pages = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    assert_eq!(
        pages.iter().map(|(_, page, _)| *page).collect::<Vec<_>>(),
        vec![61, 62, 63, 64, 65, 66],
        "Expected exactly one copy of each diagnostic page"
    );

    assert!(pages.iter().all(|(_, _, text)| !text.trim().is_empty()));

    let instructions = load_instructions(&root.join("config/answering.txt")).unwrap();
    let model = load_model().unwrap();

    println!(
        "Diagnostic: {} complete pages, {} source-text characters; \
         bypassing retrieval and its context budget.",
        pages.len(),
        pages
            .iter()
            .map(|(_, _, text)| text.chars().count())
            .sum::<usize>()
    );

    let started = std::time::Instant::now();

    let answer = answer_question(
        model.as_ref(),
        "A TDY member has a valid state driver's license but no AF Form 2293. \
         Can they check out a GOV?",
        pages.iter().map(|(document, page, text)| Source {
            document,
            page: usize::try_from(*page).expect("Page number must fit usize"),
            text,
        }),
        &instructions,
    )
    .unwrap();

    println!("\n{answer}");
    println!(
        "\nDiagnostic answer stage: {:.2}s",
        started.elapsed().as_secs_f64()
    );
}

#[test]
fn openrouter_request_separates_roles_and_preserves_input() {
    let instructions = "Use only the supplied sources.";
    let input = r#"{"user_request":"A question","sources":[]}"#;
    let model = "google/gemma-4-31b-it:free";

    let body = super::openrouter::build_request(model, instructions, input);

    assert_eq!(body["model"], model);
    assert_eq!(body["stream"], false);
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], instructions);
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], input);
}

#[test]
fn openrouter_http_errors_show_selected_details_and_redact_key() {
    let body = serde_json::json!({
        "error": {
            "code": 429,
            "message": "Capacity exceeded\ncredential=test-secret",
            "metadata": {
                "provider_name": "Example provider",
                "raw": "Raw provider data must not be printed"
            }
        }
    });

    let error = super::openrouter::http_error(
        429,
        Some("30"),
        &serde_json::to_vec(&body).unwrap(),
        "test-secret",
    )
    .to_string();

    assert!(error.contains("OpenRouter HTTP 429"));
    assert!(error.contains("API code: 429"));
    assert!(error.contains("Capacity exceeded"));
    assert!(error.contains("[REDACTED]"));
    assert!(error.contains("Provider: Example provider"));
    assert!(error.contains("Retry-After: 30"));
    assert!(!error.contains("test-secret"));
    assert!(!error.contains("Raw provider data"));
    assert!(!error.contains('\n'));

    let non_json = super::openrouter::http_error(
        503,
        None,
        b"<html>Unstructured provider response</html>",
        "test-secret",
    )
    .to_string();

    assert!(non_json.contains("OpenRouter HTTP 503"));
    assert!(!non_json.contains("<html>"));
}
