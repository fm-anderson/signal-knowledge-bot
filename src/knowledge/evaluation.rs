use super::retrieval::RetrievedDocumentPage;
use super::{
    MAX_CONTEXT_CHARS, MAX_PAGES, SqliteIndex, Terminology, build_bounded_page_context,
    load_pdf_directory, search_documents,
};
use serde::Deserialize;
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Deserialize)]
struct EvaluationSuite {
    cases: Vec<EvaluationCase>,
}

#[derive(Deserialize)]
struct EvaluationCase {
    question: String,
    expected_pages: Vec<ExpectedPage>,
}

#[derive(Deserialize, Eq, PartialEq, Hash)]
pub(super) struct ExpectedPage {
    pub(super) document: String,
    pub(super) page: usize,
}

pub(super) fn count_hits(
    expected: &[ExpectedPage],
    selected: &[RetrievedDocumentPage<'_>],
) -> usize {
    let selected_pages: HashSet<(&str, usize)> = selected
        .iter()
        .filter_map(|result| {
            result
                .document
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| (name, result.page.number))
        })
        .collect();

    expected
        .iter()
        .filter(|page| selected_pages.contains(&(page.document.as_str(), page.page)))
        .count()
}

fn print_summary(
    title: &str,
    total_cases: usize,
    cases_with_hits: usize,
    complete_cases: usize,
    total_found: usize,
    total_expected: usize,
) {
    println!("\n{title}");
    println!("Limits: up to {MAX_PAGES} pages, {MAX_CONTEXT_CHARS} characters");
    println!("Questions: {total_cases}");
    println!(
        "Questions with expected evidence: {cases_with_hits}/{total_cases} ({:.1}%)",
        100.0 * cases_with_hits as f64 / total_cases as f64
    );
    println!(
        "Complete cases: {complete_cases}/{total_cases} ({:.1}%)",
        100.0 * complete_cases as f64 / total_cases as f64
    );
    println!(
        "Delivered expected-page recall: {total_found}/{total_expected} ({:.1}%)",
        100.0 * total_found as f64 / total_expected as f64
    );
}

pub fn run_evaluation(directory: &Path, cases_path: &Path) -> Result<(), Box<dyn Error>> {
    let content = fs::read_to_string(cases_path)?;
    let suite: EvaluationSuite = serde_json::from_str(&content)?;

    if suite.cases.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Evaluation suite contains no questions",
        )
        .into());
    }

    let documents = load_pdf_directory(directory)?;
    let terminology = Terminology::load_optional(directory)?;

    // validate reference pages before calculating metrics.
    for case in &suite.cases {
        if case.question.trim().is_empty() || case.expected_pages.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Every evaluation case needs a question and expected pages",
            )
            .into());
        }

        let unique: HashSet<&ExpectedPage> = case.expected_pages.iter().collect();

        if unique.len() != case.expected_pages.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Duplicate expected page in evaluation case",
            )
            .into());
        }

        for expected in &case.expected_pages {
            let exists = documents.iter().any(|document| {
                document.path.file_name().and_then(|name| name.to_str())
                    == Some(expected.document.as_str())
                    && document
                        .pages
                        .iter()
                        .any(|page| page.number == expected.page)
            });

            if !exists {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Unknown reference: {} page {}",
                        expected.document, expected.page
                    ),
                )
                .into());
            }
        }
    }

    let mut cases_with_hits = 0;
    let mut complete_cases = 0;
    let mut total_expected = 0;
    let mut total_found = 0;

    for (index, case) in suite.cases.iter().enumerate() {
        let expanded = terminology.expand(&case.question);
        let ranked = search_documents(&documents, &expanded, MAX_PAGES);
        let context = build_bounded_page_context(&ranked, MAX_CONTEXT_CHARS);

        let hits = count_hits(&case.expected_pages, &context.pages);

        if hits > 0 {
            cases_with_hits += 1;
        }

        if hits == case.expected_pages.len() {
            complete_cases += 1;
        }

        total_expected += case.expected_pages.len();
        total_found += hits;

        println!("\n{}. {}", index + 1, case.question);
        println!(
            "   Expected pages included: {hits}/{}",
            case.expected_pages.len()
        );
        println!(
            "   Delivered: {} pages, {} characters",
            context.pages.len(),
            context.text.chars().count()
        );

        for result in &context.pages {
            println!(
                "   Retrieved: {} page {} (score {})",
                result.document.path.display(),
                result.page.number,
                result.score
            );
        }
    }

    print_summary(
        "Retrieval evaluation summary",
        suite.cases.len(),
        cases_with_hits,
        complete_cases,
        total_found,
        total_expected,
    );

    Ok(())
}

pub fn run_index_evaluation(database_path: &Path, cases_path: &Path) -> Result<(), Box<dyn Error>> {
    let content = fs::read_to_string(cases_path)?;
    let suite: EvaluationSuite = serde_json::from_str(&content)?;

    if suite.cases.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Evaluation suite contains no questions",
        )
        .into());
    }

    let index = SqliteIndex::open(database_path)?;

    let mut cases_with_hits = 0;
    let mut complete_cases = 0;
    let mut total_expected = 0;
    let mut total_found = 0;

    for (number, case) in suite.cases.iter().enumerate() {
        if case.question.trim().is_empty() || case.expected_pages.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Every case needs a question and expected pages",
            )
            .into());
        }

        let unique_expected: HashSet<_> = case
            .expected_pages
            .iter()
            .map(|page| (&page.document, page.page))
            .collect();

        if unique_expected.len() != case.expected_pages.len() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "Duplicate expected page").into(),
            );
        }

        let results = index.search(&case.question, MAX_PAGES, MAX_CONTEXT_CHARS)?;

        let selected: HashSet<(String, usize)> = results
            .pages
            .iter()
            .filter_map(|page| {
                Path::new(&page.source)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| (name.to_string(), page.page_number))
            })
            .collect();

        let hits = case
            .expected_pages
            .iter()
            .filter(|expected| selected.contains(&(expected.document.clone(), expected.page)))
            .count();

        if hits > 0 {
            cases_with_hits += 1;
        }

        if hits == case.expected_pages.len() {
            complete_cases += 1;
        }

        total_expected += case.expected_pages.len();
        total_found += hits;

        println!("\n{}. {}", number + 1, case.question);
        println!(
            "   Expected pages included: {hits}/{}",
            case.expected_pages.len()
        );
        println!(
            "   Delivered: {} pages, {} characters",
            results.pages.len(),
            results.context.chars().count()
        );

        if hits < case.expected_pages.len() {
            // Diagnostic only: inspect deeper rankings without
            // the normal page limit or context-budget restriction.
            let deeper = index.search(&case.question, 25, usize::MAX)?;

            for expected in &case.expected_pages {
                if selected.contains(&(expected.document.clone(), expected.page)) {
                    continue;
                }

                let position = deeper.pages.iter().position(|page| {
                    Path::new(&page.source)
                        .file_name()
                        .and_then(|name| name.to_str())
                        == Some(expected.document.as_str())
                        && page.page_number == expected.page
                });

                match position {
                    Some(position) => println!(
                        "   Missing: {} page {} | candidate position #{}",
                        expected.document,
                        expected.page,
                        position + 1
                    ),
                    None => println!(
                        "   Missing: {} page {} | not in top 25",
                        expected.document, expected.page
                    ),
                }
            }
        }

        for page in &results.pages {
            println!(
                "   Retrieved: {} page {} (rank {:.3})",
                page.source, page.page_number, page.rank
            );
        }
    }

    print_summary(
        "SQLite retrieval evaluation",
        suite.cases.len(),
        cases_with_hits,
        complete_cases,
        total_found,
        total_expected,
    );

    Ok(())
}
