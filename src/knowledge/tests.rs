use super::evaluation::{ExpectedPage, count_hits};
use super::index::write_database;
use super::retrieval::{chunk_document, fts_query, normalize_retrieval_text};
use super::{
    ConceptRules, PdfDocument, PdfPage, SqliteIndex, Terminology, build_bounded_page_context,
    build_page_context, search_documents, search_pages,
};
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// Retrieval

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

#[test]
fn chunks_normalize_formatting_without_changing_source_pages() {
    let original = concat!(
        "Contents........12\n",
        "Licensing………63\tMixed.…...14\n",
        "9.16.2. SF 91; DAFI24-301; 3.5 tons. ",
        "Wait… two.."
    );

    let document = PdfDocument {
        path: PathBuf::from("policy.pdf"),
        pages: vec![PdfPage {
            number: 7,
            text: original.to_string(),
        }],
    };

    let chunks = chunk_document(&document);

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].page_number, 7);
    assert_eq!(
        chunks[0].text,
        "Contents 12 Licensing 63 Mixed 14 \
     9.16.2. SF 91; DAFI24-301; 3.5 tons. Wait… two.."
    );

    assert_eq!(document.pages[0].text, original);
    assert_eq!(normalize_retrieval_text(" \n...... ………\t"), "");

    let retrieved = search_pages(&document, "licensing", 1);
    let context = build_page_context(&document, &retrieved);

    assert!(context.contains(original));
}

// Evaluation

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
#[test]
fn query_preserves_forms_and_ordinary_numbers() {
    assert_eq!(
        fts_query("SF 91"),
        Some("\"sf 91\" OR \"sf form 91\"".to_string())
    );

    assert_eq!(fts_query("DD Form 518"), fts_query("dd-518"));

    assert_eq!(
        fts_query("AF Form 2293"),
        Some("\"af 2293\" OR \"af form 2293\"".to_string())
    );

    assert_eq!(
        fts_query("DAF Form 869"),
        Some("\"daf 869\" OR \"daf form 869\"".to_string())
    );

    assert_eq!(
        fts_query("15 pax for 2 days"),
        Some("\"15\" OR \"2\" OR \"days\" OR \"pax\"".to_string())
    );

    assert_eq!(fts_query("what are the"), None);
    assert_eq!(fts_query(""), None);
}

#[test]
fn form_query_matches_phrases_without_matching_bare_numbers() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();

    connection
        .execute_batch("CREATE VIRTUAL TABLE examples USING fts5(text);")
        .unwrap();

    for text in [
        "Complete SF 91 after the incident.",
        "Complete SF Form 91 after the incident.",
        "There are 91 vehicles.",
        "Complete DD Form 91.",
        "SF guidance appears on page 91.",
    ] {
        connection
            .execute("INSERT INTO examples (text) VALUES (?1)", [text])
            .unwrap();
    }

    let query = fts_query("SF 91").unwrap();
    let mut statement = connection
        .prepare(
            "SELECT rowid FROM examples
         WHERE examples MATCH ?1
         ORDER BY rowid",
        )
        .unwrap();

    let matched: Vec<i64> = statement
        .query_map([query], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    assert_eq!(matched, vec![1, 2]);
}

// Index

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

#[test]
fn rebuilding_index_replaces_existing_content() {
    let mut connection = Connection::open_in_memory().unwrap();
    let terminology = Terminology::default();
    let concepts = ConceptRules::default();

    let original = vec![PdfDocument {
        path: PathBuf::from("old.pdf"),
        pages: vec![PdfPage {
            number: 1,
            text: "obsolete policy".to_string(),
        }],
    }];

    write_database(&mut connection, &original, &terminology, &concepts).unwrap();

    let replacement = vec![PdfDocument {
        path: PathBuf::from("new.pdf"),
        pages: vec![PdfPage {
            number: 1,
            text: "replacement guidance".to_string(),
        }],
    }];

    let count = write_database(&mut connection, &replacement, &terminology, &concepts).unwrap();

    assert_eq!(count, 1);

    let documents: Vec<String> = connection
        .prepare("SELECT path FROM documents ORDER BY path")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    assert_eq!(documents, vec!["new.pdf"]);

    for (query, expected) in [("obsolete", 0_i64), ("replacement", 1_i64)] {
        let matches: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
                [query],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(matches, expected);
    }

    let violations: i64 = connection
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();

    assert_eq!(violations, 0);
}

// Terminology

#[test]
fn terminology_expansion_improves_retrieval() {
    let terminology = Terminology::from_json(
        r#"{
            "gov": [
                "government vehicle",
                "government motor vehicle",
                "government vehicle"
            ],
            "vehicle dispatch": [
                "transportation dispatch",
                "motor vehicle dispatch"
            ]
        }"#,
    )
    .unwrap();

    let documents = vec![PdfDocument {
        path: PathBuf::from("policy.pdf"),
        pages: vec![PdfPage {
            number: 1,
            text: "Government motor vehicles require authorization.".to_string(),
        }],
    }];

    assert!(search_documents(&documents, "GOV", 3).is_empty());

    let expanded = terminology.expand("Can I use a GOV?");

    assert_eq!(
        expanded,
        "Can I use a GOV? government vehicle government motor vehicle"
    );

    let results = search_documents(&documents, &expanded, 3);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].page.number, 1);

    assert_eq!(
        terminology.expand("How does VEHICLE-DISPATCH work?"),
        "How does VEHICLE-DISPATCH work? transportation dispatch motor vehicle dispatch"
    );

    assert_eq!(terminology.expand("governor"), "governor");

    assert_eq!(
        terminology.expand("GOV government vehicle"),
        "GOV government vehicle government motor vehicle"
    );

    assert!(Terminology::from_json(r#"{"gov":"government vehicle"}"#).is_err());
}

// Concepts

#[test]
fn expands_only_when_trigger_and_context_match() {
    let rules: ConceptRules = serde_json::from_str(
        r#"{
            "rules": [{
                "any": ["defective brakes", "safety discrepancy"],
                "all": [
                    ["vehicle", "driver", "operator"],
                    ["inspection", "operating"]
                ],
                "exclude": ["bicycle"],
                "expansions": ["discontinue vehicle use"]
            }]
        }"#,
    )
    .unwrap();

    let relevant = rules.expansions_for(
        "A vehicle operator discovers defective brakes \
         during inspection. Can the vehicle continue operating?",
    );

    assert_eq!(relevant, vec!["discontinue vehicle use"]);

    // A mention of brakes alone should not activate the rule.
    assert!(
        rules
            .expansions_for("What do brake repairs cost?")
            .is_empty()
    );

    // Excluded contexts should not activate the rule.
    assert!(
        rules
            .expansions_for("A bicycle operator finds defective brakes during inspection")
            .is_empty()
    );
}
