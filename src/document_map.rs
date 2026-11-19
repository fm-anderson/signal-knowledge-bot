use rusqlite::{Connection, OpenFlags, params};
use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::path::Path;

fn open(database: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path)
}

pub fn catalog(database: &Path) -> Result<(), Box<dyn Error>> {
    let db = open(database)?;
    let mut statement = db.prepare(
        "SELECT d.path, COUNT(p.id), COALESCE(SUM(CASE WHEN TRIM(p.text) <> '' THEN 1 ELSE 0 END), 0),
                (SELECT COUNT(*) FROM chunks c JOIN pages cp ON cp.id = c.page_id WHERE cp.document_id = d.id)
         FROM documents d LEFT JOIN pages p ON p.document_id = d.id
         GROUP BY d.id ORDER BY d.path",
    )?;
    let mut rows = statement.query([])?;
    let mut total = 0usize;
    println!("Document catalog (source: indexed PDFs)");
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let pages: i64 = row.get(1)?;
        let readable: i64 = row.get(2)?;
        let chunks: i64 = row.get(3)?;
        println!(
            "  {} | {} pages ({} with text) | {} chunks",
            basename(&path),
            pages,
            readable,
            chunks
        );
        total += 1;
    }
    println!("Documents: {total}");
    println!("Use --document-outline <database.db> <document-filename> to inspect section ranges.");
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Section {
    chapter: usize,
    section: usize,
    title: String,
    start_page: i64,
    end_page: i64,
}

#[derive(Debug)]
struct Chapter {
    number: usize,
    title: Option<String>,
    start_page: i64,
    end_page: i64,
    sections: Vec<Section>,
}

fn clean_spaces(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Accept only top-level numbered sections (2.27.), never paragraph numbers (2.27.1.).
// The text after the label may contain an explanatory sentence; only its heading
// clause is retained. A bare numbered requirement is not necessarily a heading,
// so the output remains an extraction-derived candidate until reviewed.
fn section_heading(line: &str) -> Option<(usize, usize, String)> {
    let line = line.trim();
    if line.len() > 400 || line.contains("....") {
        return None;
    }
    let (prefix, rest) = line.split_once(char::is_whitespace)?;
    if !prefix.ends_with('.') {
        return None;
    }
    let numbers: Vec<&str> = prefix.trim_end_matches('.').split('.').collect();
    if numbers.len() != 2
        || numbers
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }
    let chapter: usize = numbers[0].parse().ok()?;
    let section: usize = numbers[1].parse().ok()?;
    if chapter == 0 || section == 0 || chapter > 99 || section > 999 {
        return None;
    }
    let text = clean_spaces(rest);
    if text.len() < 4 || !text.chars().any(char::is_alphabetic) {
        return None;
    }
    // The first sentence is normally the title in DAF instructions. A period
    // followed by a space is a stronger boundary than a period in e.g. 'U.S.'.
    let title = text
        .split(". ")
        .next()
        .unwrap_or(&text)
        .trim_end_matches('.');
    if title.len() < 4 {
        return None;
    }
    let title = if title.chars().count() > 110 {
        format!(
            "{}...",
            title.chars().take(107).collect::<String>().trim_end()
        )
    } else {
        title.to_string()
    };
    Some((chapter, section, title))
}

fn chapter_heading(line: &str) -> Option<(usize, Option<String>)> {
    let line = clean_spaces(line.trim());
    let mut words = line.splitn(3, ' ');
    if !words.next()?.eq_ignore_ascii_case("CHAPTER") {
        return None;
    }
    let number: usize = words
        .next()?
        .trim_end_matches([':', '-', '.'])
        .parse()
        .ok()?;
    if number == 0 || number > 99 {
        return None;
    }
    let title = words
        .next()
        .map(str::trim)
        .filter(|title| !title.is_empty());
    // Avoid treating "Chapter 1 discusses ..." in narrative text as a label.
    if title.is_some_and(|title| title.len() > 110) {
        return None;
    }
    Some((number, title.map(str::to_string)))
}

fn find_body_start(pages: &[(i64, String)]) -> i64 {
    // A contents page can contain both "CHAPTER 1" and "1.1.". Inspect enough
    // front matter to locate the *body* occurrence, including in short PDFs.
    let first_page = pages.first().map(|(page, _)| *page).unwrap_or(1);
    let early_limit = first_page + 40;
    let chapter_starts: Vec<i64> = pages
        .iter()
        .filter(|(page, _)| *page <= early_limit)
        .filter(|(_, text)| {
            text.lines()
                .any(|line| chapter_heading(line).is_some_and(|(number, _)| number == 1))
        })
        .map(|(page, _)| *page)
        .collect();
    if let Some(last) = chapter_starts.last() {
        return *last;
    }
    pages
        .iter()
        .filter(|(page, _)| *page <= early_limit)
        .filter(|(_, text)| {
            text.lines().any(|line| {
                section_heading(line)
                    .is_some_and(|(chapter, section, _)| chapter == 1 && section == 1)
            })
        })
        .map(|(page, _)| *page)
        .next_back()
        .unwrap_or(first_page)
}

fn build_outline(pages: &[(i64, String)]) -> Vec<Chapter> {
    let Some(last_page) = pages.last().map(|(page, _)| *page) else {
        return Vec::new();
    };
    let body_start = find_body_start(pages);

    // For numbered DAF instructions, N.1 is a reliable chapter anchor. A
    // reference to "CHAPTER 12" or "4.13." elsewhere must never pull a
    // chapter boundary backwards, as the previous implementation did.
    let mut chapter_starts = BTreeMap::<usize, i64>::new();
    for (page, text) in pages.iter().filter(|(page, _)| *page >= body_start) {
        for line in text.lines() {
            if let Some((chapter, section, _)) = section_heading(line)
                && section == 1
            {
                chapter_starts.entry(chapter).or_insert(*page);
            }
        }
    }

    // Ensure the chapters occur in document order. A stray reference to an
    // earlier section is discarded instead of being used as an anchor.
    let mut starts = Vec::<(usize, i64)>::new();
    let mut previous_page = 0;
    for (chapter, page) in chapter_starts {
        if page > previous_page {
            starts.push((chapter, page));
            previous_page = page;
        }
    }

    let mut output = Vec::new();
    for (position, (number, start_page)) in starts.iter().enumerate() {
        let next_start = starts.get(position + 1).map(|(_, page)| *page);
        let chapter_end = next_start
            .map(|page| (page - 1).max(*start_page))
            .unwrap_or(last_page);
        let mut title = None;
        let mut sections = BTreeMap::<usize, (i64, String)>::new();
        for (page, text) in pages
            .iter()
            .filter(|(page, _)| *page >= *start_page && next_start.is_none_or(|next| *page < next))
        {
            for line in text.lines() {
                if *page == *start_page
                    && let Some((found, chapter_title)) = chapter_heading(line)
                    && found == *number
                    && title.is_none()
                {
                    title = chapter_title;
                }
                if let Some((found, section, section_title)) = section_heading(line)
                    && found == *number
                {
                    sections.entry(section).or_insert((*page, section_title));
                }
            }
        }

        let ordered_sections: Vec<_> = sections.into_iter().collect();
        let mut resolved = Vec::new();
        for (section_position, (section, (page, section_title))) in
            ordered_sections.iter().enumerate()
        {
            // The next section may begin halfway through a page, so include
            // that shared page. This is an inclusive navigation range, not a
            // claim about the exact paragraph boundary.
            let end_page = ordered_sections
                .get(section_position + 1)
                .map(|(_, (next_page, _))| *next_page)
                .unwrap_or(chapter_end)
                .max(*page);
            resolved.push(Section {
                chapter: *number,
                section: *section,
                title: section_title.clone(),
                start_page: *page,
                end_page,
            });
        }
        output.push(Chapter {
            number: *number,
            title,
            start_page: *start_page,
            end_page: chapter_end,
            sections: resolved,
        });
    }
    output
}

pub fn outline(database: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    let db = open(database)?;
    let mut statement = db.prepare("SELECT id, path FROM documents ORDER BY path")?;
    let docs = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let matches: Vec<(i64, String)> = docs
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, path)| path == name || basename(path) == name)
        .collect();
    if matches.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Expected one matching document for '{name}', found {}",
                matches.len()
            ),
        )
        .into());
    }
    let (id, path) = &matches[0];
    let mut pages_stmt = db.prepare(
        "SELECT page_number, text FROM pages WHERE document_id = ?1 ORDER BY page_number",
    )?;
    let pages: Vec<(i64, String)> = pages_stmt
        .query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let chapters = build_outline(&pages);
    println!("Hierarchical section outline: {path}");
    println!(
        "Ranges use indexed PDF page numbers; headings are extracted candidates, not independently verified titles.\n"
    );
    let mut sections = 0usize;
    for chapter in &chapters {
        if let Some(title) = &chapter.title {
            println!(
                "Chapter {} — {} | PDF pp.{}-{}",
                chapter.number, title, chapter.start_page, chapter.end_page
            );
        } else {
            println!(
                "Chapter {} | PDF pp.{}-{}",
                chapter.number, chapter.start_page, chapter.end_page
            );
        }
        for section in &chapter.sections {
            println!(
                "  {}.{} — {} | PDF pp.{}-{}",
                section.chapter,
                section.section,
                section.title,
                section.start_page,
                section.end_page
            );
            sections += 1;
        }
    }
    println!("Chapters: {} | Sections: {sections}", chapters.len());
    if chapters.is_empty() {
        println!(
            "No numbered chapters or sections detected; this document may use a different structure."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_outline, chapter_heading, section_heading};

    #[test]
    fn parses_only_section_level_headings() {
        assert_eq!(
            section_heading(
                "2.27. Vehicle Abuse, Accidents, and Incident Maintenance. Vehicle equipment"
            ),
            Some((
                2,
                27,
                "Vehicle Abuse, Accidents, and Incident Maintenance".into()
            ))
        );
        assert_eq!(
            section_heading(
                "3.28. Vehicle Recovery. Ground Transportation normally provides this service"
            ),
            Some((3, 28, "Vehicle Recovery".into()))
        );
        assert!(section_heading("2.27.5. The operator must report accidents").is_none());
        assert!(section_heading("2.27. ..... 47").is_none());
        assert!(section_heading("An unnumbered sentence").is_none());
        assert_eq!(
            chapter_heading("CHAPTER 2 Vehicle Operations"),
            Some((2, Some("Vehicle Operations".into())))
        );
    }

    #[test]
    fn ignores_contents_and_uses_indexed_page_boundaries() {
        let pages = vec![
            (2, "CHAPTER 1\n1.1. Overview ..... 13".into()),
            (3, "2.4. Customer Service Center ... 36".into()),
            (13, "CHAPTER 1\nOverview\n1.1. Overview. Opening text\n1.2. Objective. This chapter".into()),
            (15, "CHAPTER 2 Vehicle Management\n2.1. Operations. Intro\n2.4. Customer Service Center. Customer Service Center is...".into()),
            (17, "2.27. Vehicle Abuse, Accidents, and Incident Maintenance. Explanation".into()),
            (20, "CHAPTER 3 Contingency\n3.1. Introduction. Hello".into()),
            (22, "3.28. Vehicle Recovery. Explainer".into()),
            (23, "Appendix material".into()),
        ];
        let outline = build_outline(&pages);
        assert_eq!(outline.len(), 3);
        assert_eq!((outline[0].start_page, outline[0].end_page), (13, 14));
        assert_eq!((outline[1].start_page, outline[1].end_page), (15, 19));
        // Sections 2.1 and 2.4 both start on page 15, so 2.1 ends there.
        assert_eq!(
            (
                outline[1].sections[0].start_page,
                outline[1].sections[0].end_page
            ),
            (15, 15)
        );
        assert_eq!(
            (
                outline[1].sections[1].start_page,
                outline[1].sections[1].end_page
            ),
            (15, 17)
        );
        assert_eq!(
            (
                outline[1].sections[2].start_page,
                outline[1].sections[2].end_page
            ),
            (17, 19)
        );
        assert_eq!((outline[2].start_page, outline[2].end_page), (20, 23));
    }

    #[test]
    fn ignores_early_chapter_references_and_out_of_order_sections() {
        let pages = vec![
            (1, "CHAPTER 1\n1.1. Overview ..... 14".into()),
            (14, "CHAPTER 1\n1.1. Overview. Body".into()),
            (40, "CHAPTER 2\n2.1. Operations. Body".into()),
            (
                48,
                "4.13. This is a cross reference, not a chapter start".into(),
            ),
            (
                69,
                "CHAPTER 4\n4.1. Roles and Responsibilities. Body".into(),
            ),
            (81, "4.13. Vehicle License Plate Program. Body".into()),
            (120, "CHAPTER 12 is referenced here".into()),
            (159, "CHAPTER 12\n12.1. Overview. Body".into()),
            (160, "12.5. Responsibilities. Body".into()),
        ];
        let outline = build_outline(&pages);
        assert_eq!(
            outline
                .iter()
                .map(|chapter| (chapter.number, chapter.start_page))
                .collect::<Vec<_>>(),
            vec![(1, 14), (2, 40), (4, 69), (12, 159)]
        );
        let chapter_four = &outline[2];
        assert_eq!(
            chapter_four
                .sections
                .iter()
                .find(|section| section.section == 13)
                .unwrap()
                .start_page,
            81
        );
        assert_eq!(outline[3].end_page, 160);
    }
}
