use super::retrieval::{chunk_document, keywords};
use super::{ConceptRules, PdfDocument, Terminology, load_pdf_directory};

use rusqlite::{Connection, OpenFlags, params};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io;
use std::path::Path;

const SCHEMA_VERSION: i64 = 1;

pub struct SqliteIndex {
    connection: Connection,
    terminology: Terminology,
    concepts: ConceptRules,
}

pub struct IndexedPage {
    pub source: String,
    pub page_number: usize,
    pub rank: f64,
    pub text: String,
}

pub struct SearchResult {
    pub expanded_question: String,
    pub pages: Vec<IndexedPage>,
    pub context: String,
}

impl SqliteIndex {
    pub fn build(directory: &Path, database_path: &Path) -> Result<(usize, usize), Box<dyn Error>> {
        let documents = load_pdf_directory(directory)?;
        let terminology = Terminology::load_optional(directory)?;
        let concepts = ConceptRules::load_optional(directory)?;

        let mut connection = Connection::open(database_path)?;
        let chunks = write_database(&mut connection, &documents, &terminology, &concepts)?;

        Ok((documents.len(), chunks))
    }

    pub fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if version != SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported index version. Rebuild the index.",
            )
            .into());
        }

        let terminology_json: String = connection.query_row(
            "SELECT value FROM settings WHERE key = 'terminology'",
            [],
            |row| row.get(0),
        )?;
        let terminology = serde_json::from_str(&terminology_json)?;

        let concepts_json: String = connection.query_row(
            "SELECT value FROM settings WHERE key = 'concept_rules'",
            [],
            |row| row.get(0),
        )?;
        let concepts: ConceptRules = serde_json::from_str(&concepts_json)?;

        Ok(Self {
            connection,
            terminology,
            concepts,
        })
    }

    // search indexed chunks, return complete pages.
    pub fn search(
        &self,
        question: &str,
        limit: usize,
        max_chars: usize,
    ) -> rusqlite::Result<SearchResult> {
        let mut expanded_question = self.terminology.expand(question);

        for expansion in self.concepts.expansions_for(question) {
            expanded_question.push(' ');
            expanded_question.push_str(&expansion);
        }

        let mut result = SearchResult {
            expanded_question: expanded_question.clone(),
            pages: Vec::new(),
            context: String::new(),
        };

        if limit == 0 || max_chars == 0 {
            return Ok(result);
        }

        let mut terms: Vec<String> = keywords(&expanded_question).into_iter().collect();

        terms.sort();

        if terms.is_empty() {
            return Ok(result);
        }

        let query = terms
            .iter()
            .map(|term| format!("\"{term}\""))
            .collect::<Vec<_>>()
            .join(" OR ");

        let mut statement = self.connection.prepare(
            "SELECT chunks.page_id, chunks_fts.rank
             FROM chunks_fts
             JOIN chunks ON chunks.id = chunks_fts.rowid
             WHERE chunks_fts MATCH ?1
             ORDER BY chunks_fts.rank",
        )?;

        let mut rows = statement.query([query])?;
        let mut seen_pages = HashSet::new();
        let mut used_chars = 0;

        while let Some(row) = rows.next()? {
            let page_id: i64 = row.get(0)?;
            let rank: f64 = row.get(1)?;

            if !seen_pages.insert(page_id) {
                continue;
            }

            let mut page: IndexedPage = self.connection.query_row(
                "SELECT documents.path, pages.page_number, pages.text
                 FROM pages
                 JOIN documents ON documents.id = pages.document_id
                 WHERE pages.id = ?1",
                [page_id],
                |row| {
                    Ok(IndexedPage {
                        source: row.get(0)?,
                        page_number: row.get::<_, i64>(1)? as usize,
                        text: row.get(2)?,
                        rank: 0.0,
                    })
                },
            )?;

            page.rank = rank;

            let entry = format!(
                "[Document: {} | PDF page {}]\n{}\n\n",
                page.source, page.page_number, page.text
            );

            let entry_chars = entry.chars().count();

            if entry_chars > max_chars.saturating_sub(used_chars) {
                continue;
            }

            result.context.push_str(&entry);
            used_chars += entry_chars;
            result.pages.push(page);

            if result.pages.len() == limit {
                break;
            }
        }

        Ok(result)
    }
}

fn write_database(
    connection: &mut Connection,
    documents: &[PdfDocument],
    terminology: &Terminology,
    concepts: &ConceptRules,
) -> Result<usize, Box<dyn Error>> {
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;

    let transaction = connection.transaction()?;

    transaction.execute_batch(
        "
        DROP TABLE IF EXISTS chunks_fts;
        DROP TABLE IF EXISTS chunks;
        DROP TABLE IF EXISTS pages;
        DROP TABLE IF EXISTS documents;
        DROP TABLE IF EXISTS settings;

        CREATE TABLE documents (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE
        );

        CREATE TABLE pages (
            id INTEGER PRIMARY KEY,
            document_id INTEGER NOT NULL
                REFERENCES documents(id),
            page_number INTEGER NOT NULL,
            text TEXT NOT NULL,
            UNIQUE(document_id, page_number)
        );

        CREATE TABLE chunks (
            id INTEGER PRIMARY KEY,
            page_id INTEGER NOT NULL REFERENCES pages(id)
        );

        CREATE INDEX idx_chunks_page ON chunks(page_id);

        CREATE VIRTUAL TABLE chunks_fts USING fts5(
            text,
            tokenize = 'unicode61'
        );

        CREATE TABLE settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        PRAGMA user_version = 1;
        ",
    )?;

    let terminology_json = serde_json::to_string(terminology)?;

    transaction.execute(
        "INSERT INTO settings (key, value)
         VALUES ('terminology', ?1)",
        [terminology_json],
    )?;

    let concepts_json = serde_json::to_string(concepts)?;

    transaction.execute(
        "INSERT INTO settings (key, value)
     VALUES ('concept_rules', ?1)",
        [concepts_json],
    )?;

    let mut total_chunks = 0;

    for document in documents {
        let path = document.path.to_string_lossy();

        transaction.execute("INSERT INTO documents (path) VALUES (?1)", [path.as_ref()])?;

        let document_id = transaction.last_insert_rowid();
        let mut page_ids = HashMap::new();

        for page in &document.pages {
            transaction.execute(
                "INSERT INTO pages (document_id, page_number, text)
                 VALUES (?1, ?2, ?3)",
                params![document_id, page.number as i64, page.text],
            )?;

            page_ids.insert(page.number, transaction.last_insert_rowid());
        }

        // index chunks with link to complete pages.
        for chunk in chunk_document(document) {
            let page_id = page_ids[&chunk.page_number];

            transaction.execute("INSERT INTO chunks (page_id) VALUES (?1)", [page_id])?;

            let chunk_id = transaction.last_insert_rowid();

            transaction.execute(
                "INSERT INTO chunks_fts (rowid, text)
                 VALUES (?1, ?2)",
                params![chunk_id, chunk.text],
            )?;

            total_chunks += 1;
        }
    }

    transaction.commit()?;

    Ok(total_chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::PdfPage;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn sqlite_index_retrieves_complete_pages() {
        let filler = (0..220)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");

        let complete_page = format!("government vehicle {filler} authorization");

        let documents = vec![PdfDocument {
            path: PathBuf::from("policy.pdf"),
            pages: vec![
                PdfPage {
                    number: 1,
                    text: complete_page.clone(),
                },
                PdfPage {
                    number: 2,
                    text: "Unrelated material.".to_string(),
                },
            ],
        }];

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let path =
            std::env::temp_dir().join(format!("signal-index-{}-{unique}.db", std::process::id()));

        let mut connection = Connection::open(&path).unwrap();

        let count = write_database(
            &mut connection,
            &documents,
            &Terminology::default(),
            &ConceptRules::default(),
        )
        .unwrap();

        assert_eq!(count, 3);

        drop(connection);

        let index = SqliteIndex::open(&path).unwrap();

        let result = index.search("vehicle authorization", 5, 10_000).unwrap();

        assert_eq!(result.pages.len(), 1);
        assert_eq!(result.pages[0].page_number, 1);
        assert_eq!(result.context.matches("[Document:").count(), 1);

        assert!(result.context.contains(&complete_page));

        let too_small = index.search("vehicle authorization", 5, 10).unwrap();

        assert!(too_small.pages.is_empty());

        drop(index);
        std::fs::remove_file(path).unwrap();
    }
}
