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

pub(super) fn normalize_retrieval_text(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();

    while let Some(character) = characters.next() {
        if matches!(character, '.' | '…') {
            let mut run = String::new();
            run.push(character);

            while let Some(&next) = characters.peek() {
                if !matches!(next, '.' | '…') {
                    break;
                }

                run.push(next);
                characters.next();
            }

            if run.chars().count() >= 3 {
                cleaned.push(' ');
            } else {
                cleaned.push_str(&run);
            }
        } else {
            cleaned.push(character);
        }
    }

    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn chunk_document(document: &PdfDocument) -> Vec<DocumentChunk> {
    let mut chunks = Vec::new();

    for page in &document.pages {
        let normalized = normalize_retrieval_text(&page.text);
        let words: Vec<&str> = normalized.split_whitespace().collect();
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

pub(super) fn keywords(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| {
            !word.is_empty()
                && (word.len() >= 3 || word.chars().all(|character| character.is_ascii_digit()))
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

pub(super) fn fts_query(text: &str) -> Option<String> {
    let lowercase = text.to_lowercase();
    let words: Vec<&str> = lowercase
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();

    let mut terms = HashSet::new();
    let mut remaining_words = Vec::new();
    let mut position = 0;

    while position < words.len() {
        let prefix = words[position];

        if matches!(prefix, "sf" | "dd" | "af" | "daf") {
            let mut number_position = position + 1;

            if words.get(number_position) == Some(&"form") {
                number_position += 1;
            }

            if let Some(&number) = words.get(number_position)
                && number.chars().all(|character| character.is_ascii_digit())
            {
                terms.insert(format!("{prefix} {number}"));
                terms.insert(format!("{prefix} form {number}"));

                position = number_position + 1;
                continue;
            }
        }

        remaining_words.push(words[position]);
        position += 1;
    }

    terms.extend(keywords(&remaining_words.join(" ")));

    if terms.is_empty() {
        return None;
    }

    let mut terms: Vec<String> = terms.into_iter().collect();
    terms.sort();

    Some(
        terms
            .iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
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
