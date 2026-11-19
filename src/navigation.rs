//! Read-only navigation metadata. Summaries are discovery hints, never evidence.
use rusqlite::{Connection, OptionalExtension, params};
use std::io;

#[derive(Clone, Debug)]
pub struct Document {
    pub path: String,
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub chapter: i64,
    pub number: i64,
    pub title: String,
    pub start_page: usize,
    pub end_page: usize,
    pub summary: String,
    pub excerpt_truncated: Option<bool>,
}

fn table_exists(db: &Connection, name: &str) -> rusqlite::Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn documents(db: &Connection) -> io::Result<Vec<Document>> {
    if !table_exists(db, "navigation_document_summaries").map_err(io::Error::other)? {
        return Ok(Vec::new());
    }
    let mut stmt = db
        .prepare(
            "SELECT d.path, n.summary FROM navigation_document_summaries n
         JOIN documents d ON d.id=n.document_id ORDER BY d.path LIMIT 25",
        )
        .map_err(io::Error::other)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Document {
                path: row.get(0)?,
                summary: row.get(1)?,
            })
        })
        .map_err(io::Error::other)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(io::Error::other)
}

pub fn sections(db: &Connection, document: &str) -> io::Result<Vec<Section>> {
    if !table_exists(db, "navigation_section_summaries").map_err(io::Error::other)? {
        return Ok(Vec::new());
    }
    // Only section-aware summaries are eligible for navigation. No implicit
    // fallback to older, page-prefix summaries.
    let mut stmt = db
        .prepare(
            "SELECT s.chapter, s.section, s.title, s.start_page, s.end_page,
                s.summary, s.excerpt_truncated
         FROM navigation_section_summaries s JOIN documents d ON d.id=s.document_id
         WHERE d.path=?1 AND s.excerpt_version=1
         ORDER BY s.chapter, s.section LIMIT 120",
        )
        .map_err(io::Error::other)?;
    let rows = stmt
        .query_map([document], |row| {
            let start: i64 = row.get(3)?;
            let end: i64 = row.get(4)?;
            if start < 1 || end < start {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Ok(Section {
                chapter: row.get(0)?,
                number: row.get(1)?,
                title: row.get(2)?,
                start_page: usize::try_from(start).map_err(|_| rusqlite::Error::InvalidQuery)?,
                end_page: usize::try_from(end).map_err(|_| rusqlite::Error::InvalidQuery)?,
                summary: row.get(5)?,
                excerpt_truncated: row.get::<_, Option<i64>>(6)?.map(|x| x != 0),
            })
        })
        .map_err(io::Error::other)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(io::Error::other)
}

pub fn read_page(db: &Connection, document: &str, page: usize) -> io::Result<Option<String>> {
    let number = i64::try_from(page).map_err(io::Error::other)?;
    db.query_row(
        "SELECT p.text FROM pages p JOIN documents d ON d.id=p.document_id
         WHERE d.path=?1 AND p.page_number=?2",
        params![document, number],
        |row| row.get(0),
    )
    .optional()
    .map_err(io::Error::other)
}
