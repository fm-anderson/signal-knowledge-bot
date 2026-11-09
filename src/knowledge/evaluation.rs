use super::SqliteIndex;
use super::retrieval::RetrievedDocumentPage;
use super::{Terminology, build_bounded_page_context, load_pdf_directory, search_documents};
use serde::Deserialize;
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

const TOP_K: usize = 5;
const MAX_CONTEXT_CHARS: usize = 20_000;

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
struct ExpectedPage {
    document: String,
    page: usize,
}

fn count_hits(expected: &[ExpectedPage], selected: &[RetrievedDocumentPage<'_>]) -> usize {
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
    let mut total_expected = 0;
    let mut total_found = 0;

    for (index, case) in suite.cases.iter().enumerate() {
        let expanded = terminology.expand(&case.question);

        let ranked = search_documents(&documents, &expanded, TOP_K);

        let context = build_bounded_page_context(&ranked, MAX_CONTEXT_CHARS);

        let hits = count_hits(&case.expected_pages, &context.pages);

        if hits > 0 {
            cases_with_hits += 1;
        }

        total_expected += case.expected_pages.len();
        total_found += hits;

        println!("\n{}. {}", index + 1, case.question);

        println!(
            "   Expected pages included: {}/{}",
            hits,
            case.expected_pages.len()
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

    let total_cases = suite.cases.len();

    let hit_rate = cases_with_hits as f64 / total_cases as f64 * 100.0;

    let page_recall = total_found as f64 / total_expected as f64 * 100.0;

    println!("\nRetrieval evaluation summary");
    println!("Questions: {total_cases}");

    println!("Hit@{TOP_K}: {cases_with_hits}/{total_cases} ({hit_rate:.1}%)");

    println!("Page Recall@{TOP_K}: {total_found}/{total_expected} ({page_recall:.1}%)");

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

        let results = index.search(&case.question, 5, 20_000)?;

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

        total_expected += case.expected_pages.len();
        total_found += hits;

        println!("\n{}. {}", number + 1, case.question);
        println!(
            "   Expected pages included: {hits}/{}",
            case.expected_pages.len()
        );

        if hits < case.expected_pages.len() {
            // Diagnostic only: inspect deeper rankings without
            // the normal five-page or context-budget restriction.
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
                    Some(index) => println!(
                        "   Missing: {} page {} | candidate position #{}",
                        expected.document,
                        expected.page,
                        index + 1
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

    let total_cases = suite.cases.len();

    println!("\nSQLite retrieval evaluation");
    println!("Questions: {total_cases}");
    println!(
        "Hit@5: {}/{} ({:.1}%)",
        cases_with_hits,
        total_cases,
        100.0 * cases_with_hits as f64 / total_cases as f64
    );
    println!(
        "Page Recall@5: {}/{} ({:.1}%)",
        total_found,
        total_expected,
        100.0 * total_found as f64 / total_expected as f64
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{PdfDocument, PdfPage};
    use std::path::PathBuf;

    #[test]
    fn evaluation_scores_only_pages_in_context() {
        let documents = vec![
            PdfDocument {
                path: PathBuf::from("a.pdf"),
                pages: vec![PdfPage {
                    number: 1,
                    text: "Government vehicles require authorization.".to_string(),
                }],
            },
            PdfDocument {
                path: PathBuf::from("b.pdf"),
                pages: vec![PdfPage {
                    number: 2,
                    text: "Government vehicle authorization rules.".to_string(),
                }],
            },
        ];

        let expected = vec![
            ExpectedPage {
                document: "a.pdf".to_string(),
                page: 1,
            },
            ExpectedPage {
                document: "b.pdf".to_string(),
                page: 2,
            },
        ];

        let ranked = search_documents(&documents, "government vehicle authorization", 5);

        assert_eq!(ranked.len(), 2);

        let empty_context = build_bounded_page_context(&ranked, 10);

        assert_eq!(count_hits(&expected, &empty_context.pages), 0);

        let full_context = build_bounded_page_context(&ranked, 10_000);

        assert_eq!(count_hits(&expected, &full_context.pages), 2);
    }
}
