use super::{PdfDocument, PdfPage};
use std::collections::{HashMap, HashSet};

const CHUNK_WORDS: usize = 180;
const CHUNK_OVERLAP: usize = 30;

pub struct DocumentChunk {
    pub page_number: usize,
    pub text: String,
}

pub struct RetrievedPage<'a> {
    pub page: &'a PdfPage,
    pub score: usize,
}

#[derive(Clone, Copy)]
pub struct RetrievedDocumentPage<'a> {
    pub document: &'a PdfDocument,
    pub page: &'a PdfPage,
    pub score: usize,
}

pub struct PageContext<'a> {
    pub text: String,
    pub pages: Vec<RetrievedDocumentPage<'a>>,
}

// overlapping searchable chunks.
pub fn chunk_document(document: &PdfDocument) -> Vec<DocumentChunk> {
    let mut chunks = Vec::new();

    for page in &document.pages {
        let words: Vec<&str> = page.text.split_whitespace().collect();
        let mut start = 0;

        while start < words.len() {
            let end = (start + CHUNK_WORDS).min(words.len());

            chunks.push(DocumentChunk {
                page_number: page.number,
                text: words[start..end].join(" "),
            });

            if end == words.len() {
                break;
            }

            start = end - CHUNK_OVERLAP;
        }
    }

    chunks
}

// keyword extraction for retrieval baseline.
fn keywords(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| {
            word.len() >= 3
                && !matches!(
                    *word,
                    "the"
                        | "and"
                        | "for"
                        | "with"
                        | "from"
                        | "that"
                        | "this"
                        | "can"
                        | "are"
                        | "what"
                        | "how"
                )
        })
        .map(str::to_string)
        .collect()
}

pub fn search_pages<'a>(
    document: &'a PdfDocument,
    question: &str,
    limit: usize,
) -> Vec<RetrievedPage<'a>> {
    let query_terms = keywords(question);
    let mut page_scores: HashMap<usize, usize> = HashMap::new();

    if query_terms.is_empty() || limit == 0 {
        return Vec::new();
    }

    for chunk in chunk_document(document) {
        let chunk_terms = keywords(&chunk.text);

        let score = query_terms.intersection(&chunk_terms).count();

        if score == 0 {
            continue;
        }

        page_scores
            .entry(chunk.page_number)
            .and_modify(|best| *best = (*best).max(score))
            .or_insert(score);
    }

    let mut ranked: Vec<(usize, usize)> = page_scores.into_iter().collect();

    // highest score first; page number breaks ties.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    ranked
        .into_iter()
        .take(limit)
        .filter_map(|(number, score)| {
            document
                .pages
                .iter()
                .find(|page| page.number == number)
                .map(|page| RetrievedPage { page, score })
        })
        .collect()
}

pub fn search_documents<'a>(
    documents: &'a [PdfDocument],
    question: &str,
    limit: usize,
) -> Vec<RetrievedDocumentPage<'a>> {
    let mut results = Vec::new();

    for document in documents {
        for result in search_pages(document, question, limit) {
            results.push(RetrievedDocumentPage {
                document,
                page: result.page,
                score: result.score,
            });
        }
    }

    results.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.document.path.cmp(&b.document.path))
            .then_with(|| a.page.number.cmp(&b.page.number))
    });

    results.truncate(limit);
    results
}

pub fn build_page_context(document: &PdfDocument, pages: &[RetrievedPage<'_>]) -> String {
    let mut context = String::new();

    for result in pages {
        context.push_str(&format!(
            "[Document: {} | PDF page {}]\n",
            document.path.display(),
            result.page.number
        ));

        context.push_str(&result.page.text);
        context.push_str("\n\n");
    }

    context
}

pub fn build_bounded_page_context<'a>(
    ranked_pages: &[RetrievedDocumentPage<'a>],
    max_chars: usize,
) -> PageContext<'a> {
    let mut context = String::new();
    let mut selected = Vec::new();
    let mut used_chars = 0;

    for result in ranked_pages {
        let entry = format!(
            "[Document: {} | PDF page {}]\n{}\n\n",
            result.document.path.display(),
            result.page.number,
            result.page.text
        );

        let entry_chars = entry.chars().count();

        if entry_chars > max_chars.saturating_sub(used_chars) {
            continue;
        }

        context.push_str(&entry);
        used_chars += entry_chars;
        selected.push(*result);
    }

    PageContext {
        text: context,
        pages: selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn retrieval_uses_chunks_but_returns_complete_pages() {
        let filler = (0..400)
            .map(|i| format!("filler{i}"))
            .collect::<Vec<_>>()
            .join(" ");

        let long_page = format!("vehicle {filler} authorization");

        let document = PdfDocument {
            path: PathBuf::from("policy.pdf"),
            pages: vec![
                PdfPage {
                    number: 1,
                    text: long_page.clone(),
                },
                PdfPage {
                    number: 2,
                    text: String::new(),
                },
                PdfPage {
                    number: 3,
                    text: "Vehicle authorization requirements.".to_string(),
                },
            ],
        };

        let chunks = chunk_document(&document);

        assert_eq!(chunks.len(), 4);

        let first: Vec<_> = chunks[0].text.split_whitespace().collect();

        let second: Vec<_> = chunks[1].text.split_whitespace().collect();

        assert_eq!(&first[150..], &second[..30]);

        let results = search_pages(&document, "vehicle authorization", 3);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].page.number, 3);
        assert_eq!(results[1].page.number, 1);

        let context = build_page_context(&document, &results);

        assert!(context.contains(&long_page));

        assert_eq!(context.matches("[Document:").count(), 2);

        assert!(search_pages(&document, "helicopter", 3).is_empty());
    }

    #[test]
    fn searches_multiple_documents_with_complete_page_budget() {
        let filler = (0..220)
            .map(|i| format!("filler{i}"))
            .collect::<Vec<_>>()
            .join(" ");

        let complete_page = format!("vehicle authorization {filler} final-exception");

        let documents = vec![
            PdfDocument {
                path: PathBuf::from("a.pdf"),
                pages: vec![
                    PdfPage {
                        number: 1,
                        text: "vehicle maintenance".to_string(),
                    },
                    PdfPage {
                        number: 2,
                        text: complete_page.clone(),
                    },
                ],
            },
            PdfDocument {
                path: PathBuf::from("b.pdf"),
                pages: vec![PdfPage {
                    number: 1,
                    text: "vehicle authorization limitations".to_string(),
                }],
            },
        ];

        let ranked = search_documents(&documents, "vehicle authorization", 2);

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].document.path, PathBuf::from("a.pdf"));
        assert_eq!(ranked[0].page.number, 2);
        assert_eq!(ranked[1].document.path, PathBuf::from("b.pdf"));

        let first_entry = format!("[Document: a.pdf | PDF page 2]\n{}\n\n", complete_page);

        let context = build_bounded_page_context(&ranked, first_entry.chars().count());

        assert_eq!(context.pages.len(), 1);
        assert_eq!(context.text, first_entry);

        assert!(context.text.contains("final-exception"));

        let too_small = build_bounded_page_context(&ranked, 10);

        assert!(too_small.pages.is_empty());
    }
}
