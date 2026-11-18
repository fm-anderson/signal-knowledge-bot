use crate::knowledge::SqliteIndex;
use serde::Deserialize;
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

const MAX_PAGES: usize = 10;
const MAX_CHARS: usize = 20_000;
const DIAGNOSTIC_LIMIT: usize = 25;

#[derive(Deserialize)]
struct Suite {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    question: String,
    expected_pages: Vec<Expected>,
}

#[derive(Deserialize, Clone, PartialEq, Eq, Hash)]
struct Expected {
    document: String,
    page: usize,
}

fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

pub fn run(database: &Path, evaluation: &Path) -> Result<(), Box<dyn Error>> {
    let suite: Suite = serde_json::from_str(&fs::read_to_string(evaluation)?)?;
    if suite.cases.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Empty evaluation suite").into());
    }

    let index = SqliteIndex::open(database)?;
    let mut questions_with_evidence = 0usize;
    let mut complete_cases = 0usize;
    let mut found_total = 0usize;
    let mut expected_total = 0usize;

    for (number, case) in suite.cases.iter().enumerate() {
        let unique: HashSet<_> = case.expected_pages.iter().collect();
        if case.question.trim().is_empty()
            || case.expected_pages.is_empty()
            || unique.len() != case.expected_pages.len()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid or duplicate expected pages in case {}", number + 1),
            )
            .into());
        }

        let results = index.search_context(&case.question, MAX_PAGES, MAX_CHARS)?;
        let selected: HashSet<(&str, usize)> = results
            .pages
            .iter()
            .map(|page| (basename(&page.source), page.page_number))
            .collect();
        let hits = case
            .expected_pages
            .iter()
            .filter(|item| selected.contains(&(item.document.as_str(), item.page)))
            .count();

        questions_with_evidence += usize::from(hits > 0);
        complete_cases += usize::from(hits == case.expected_pages.len());
        found_total += hits;
        expected_total += case.expected_pages.len();

        println!("\n{}. {}", number + 1, case.question);
        println!(
            "   Expected pages included: {hits}/{} | Delivered: {} pages, {} characters",
            case.expected_pages.len(),
            results.pages.len(),
            results.context.chars().count()
        );

        if hits < case.expected_pages.len() {
            let deeper = index.search_context(&case.question, DIAGNOSTIC_LIMIT, usize::MAX)?;
            for expected in &case.expected_pages {
                if selected.contains(&(expected.document.as_str(), expected.page)) {
                    continue;
                }
                let position = deeper.pages.iter().position(|page| {
                    basename(&page.source) == expected.document && page.page_number == expected.page
                });
                match position {
                    Some(position) => println!(
                        "   Missing: {} page {} | candidate position #{}",
                        expected.document,
                        expected.page,
                        position + 1
                    ),
                    None => println!(
                        "   Missing: {} page {} | not in top {}",
                        expected.document, expected.page, DIAGNOSTIC_LIMIT
                    ),
                }
            }
        }
        for page in &results.pages {
            println!(
                "   Retrieved: {} page {} (context BM25 {:.3})",
                page.source, page.page_number, page.rank
            );
        }
    }

    let n = suite.cases.len();
    println!("\nContextual FTS5 retrieval evaluation");
    println!("Limits: up to {MAX_PAGES} pages, {MAX_CHARS} characters");
    println!("Questions: {n}");
    println!(
        "Questions with expected evidence: {questions_with_evidence}/{n} ({:.1}%)",
        100.0 * questions_with_evidence as f64 / n as f64
    );
    println!(
        "Complete cases: {complete_cases}/{n} ({:.1}%)",
        100.0 * complete_cases as f64 / n as f64
    );
    println!(
        "Delivered expected-page recall: {found_total}/{expected_total} ({:.1}%)",
        100.0 * found_total as f64 / expected_total as f64
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::basename;

    #[test]
    fn basename_matches_evaluation_document_ids() {
        assert_eq!(basename("manuals/dafi24-301.pdf"), "dafi24-301.pdf");
        assert_eq!(basename("dafi24-301.pdf"), "dafi24-301.pdf");
    }
}
