mod bot;
mod knowledge;
mod signal;

use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some(command) = args.first().map(String::as_str) else {
        eprintln!(
            "Usage: signal-knowledge-bot <message | --signal-json | --mock-daemon | --socket-client>"
        );
        return;
    };

    match command {
        "--mock-daemon" | "--socket-client" => {
            let Some(path) = args.get(1) else {
                eprintln!("Missing socket path");
                return;
            };

            let path = Path::new(path);

            let result = if command == "--mock-daemon" {
                signal::run_mock_daemon(path)
            } else {
                signal::run_socket_client(path)
            };

            if let Err(error) = result {
                eprintln!("Socket error: {error}");
            }
        }

        "--signal-json" => {
            let raw_json = args[1..].join(" ");

            match signal::parse_signal_notification(&raw_json) {
                Some(incoming) => {
                    let outgoing = bot::handle_message(&incoming);

                    println!("Destination: {:?}", outgoing.destination);
                    println!("Reply: {}", outgoing.text);
                }
                None => {
                    eprintln!("Invalid or unsupported Signal notification");
                }
            }
        }

        "--inspect-pdf" => {
            let Some(path) = args.get(1) else {
                eprintln!("Usage: signal-knowledge-bot --inspect-pdf <file.pdf>");
                return;
            };

            match knowledge::load_pdf(Path::new(path)) {
                Ok(document) => {
                    println!("Document: {}", document.path.display());
                    println!("Total pages: {}", document.pages.len());

                    let text_pages = document
                        .pages
                        .iter()
                        .filter(|page| !page.text.trim().is_empty())
                        .count();

                    println!("Pages containing text: {text_pages}");

                    for page in document
                        .pages
                        .iter()
                        .filter(|page| !page.text.trim().is_empty())
                        .take(3)
                    {
                        let preview = page
                            .text
                            .split_whitespace()
                            .take(30)
                            .collect::<Vec<_>>()
                            .join(" ");

                        println!("Page {}: {preview}...", page.number);
                    }

                    if text_pages == 0 {
                        eprintln!("No extractable text found. The PDF may require OCR.");
                    }
                }
                Err(error) => eprintln!("PDF extraction failed: {error}"),
            }
        }

        "--search-pdf" => {
            if args.len() < 3 {
                eprintln!("Usage: signal-knowledge-bot --search-pdf <file.pdf> <question>");
                return;
            }

            let path = Path::new(&args[1]);
            let question = args[2..].join(" ");

            match knowledge::load_pdf(path) {
                Ok(document) => {
                    let results = knowledge::search_pages(&document, &question, 3);

                    if results.is_empty() {
                        println!("No matching pages found.");
                        return;
                    }

                    println!("Question: {question}");
                    println!("Relevant pages:");

                    for result in &results {
                        println!(
                            "  PDF page {} (score: {})",
                            result.page.number, result.score
                        );
                    }

                    let context = knowledge::build_page_context(&document, &results);

                    println!(
                        "LLM context prepared: {} complete pages, {} characters",
                        results.len(),
                        context.chars().count()
                    );
                }
                Err(error) => eprintln!("PDF search failed: {error}"),
            }
        }

        "--search-dir" => {
            if args.len() < 3 {
                eprintln!("Usage: signal-knowledge-bot --search-dir <directory> <question>");
                return;
            }

            let directory = Path::new(&args[1]);
            let question = args[2..].join(" ");

            let terminology = match knowledge::Terminology::load_optional(directory) {
                Ok(terminology) => terminology,
                Err(error) => {
                    eprintln!("Failed to load terminology: {error}");
                    return;
                }
            };

            let expanded_question = terminology.expand(&question);

            use knowledge::{MAX_CONTEXT_CHARS, MAX_PAGES};

            match knowledge::load_pdf_directory(directory) {
                Ok(documents) => {
                    println!("Loaded {} PDF documents", documents.len());

                    let ranked =
                        knowledge::search_documents(&documents, &expanded_question, MAX_PAGES);

                    let context = knowledge::build_bounded_page_context(&ranked, MAX_CONTEXT_CHARS);

                    if context.pages.is_empty() {
                        println!("No matching pages fit the context budget.");
                        return;
                    }

                    println!("Question: {question}");

                    if expanded_question != question {
                        println!("Expanded query: {expanded_question}");
                    }

                    for result in &context.pages {
                        println!(
                            "  {} | PDF page {} | Score {}",
                            result.document.path.display(),
                            result.page.number,
                            result.score
                        );
                    }

                    println!(
                        "LLM context: {} complete pages, {} characters",
                        context.pages.len(),
                        context.text.chars().count()
                    );

                    if context.pages.len() < ranked.len() {
                        println!("Some matching pages were excluded by the context budget.");
                    }
                }
                Err(error) => eprintln!("Document loading failed: {error}"),
            }
        }

        "--eval-retrieval" => {
            if args.len() != 3 {
                eprintln!(
                    "Usage: signal-knowledge-bot --eval-retrieval <manuals-dir> <evaluation.json>"
                );
                return;
            }

            let directory = Path::new(&args[1]);
            let cases_path = Path::new(&args[2]);

            if let Err(error) = knowledge::run_evaluation(directory, cases_path) {
                eprintln!("Retrieval evaluation failed: {error}");
            }
        }

        "--build-index" => {
            if args.len() != 3 {
                eprintln!("Usage: signal-knowledge-bot --build-index <manuals-dir> <database.db>");
                return;
            }

            let started = Instant::now();

            match knowledge::SqliteIndex::build(Path::new(&args[1]), Path::new(&args[2])) {
                Ok((documents, chunks)) => {
                    println!("SQLite index saved: {}", args[2]);
                    println!("Documents: {documents}");
                    println!("Searchable chunks: {chunks}");
                    println!("Indexing time: {:?}", started.elapsed());
                }
                Err(error) => eprintln!("Indexing failed: {error}"),
            }
        }

        "--search-index" => {
            if args.len() < 3 {
                eprintln!("Usage: signal-knowledge-bot --search-index <database.db> <question>");
                return;
            }

            let load_start = Instant::now();

            let index = match knowledge::SqliteIndex::open(Path::new(&args[1])) {
                Ok(index) => index,
                Err(error) => {
                    eprintln!("Failed to open index: {error}");
                    return;
                }
            };

            let load_time = load_start.elapsed();
            let search_start = Instant::now();

            match index.search(
                &args[2..].join(" "),
                knowledge::MAX_PAGES,
                knowledge::MAX_CONTEXT_CHARS,
            ) {
                Ok(results) => {
                    println!("Expanded query: {}", results.expanded_question);

                    for page in &results.pages {
                        println!(
                            "  {} | PDF page {} | BM25 rank {:.5}",
                            page.source, page.page_number, page.rank
                        );
                    }

                    println!(
                        "LLM context: {} complete pages, {} characters",
                        results.pages.len(),
                        results.context.chars().count()
                    );
                }
                Err(error) => eprintln!("Index search failed: {error}"),
            }

            println!("Index load time: {load_time:?}");
            println!("Search and context time: {:?}", search_start.elapsed());
        }

        "--eval-index" => {
            if args.len() != 3 {
                eprintln!(
                    "Usage: signal-knowledge-bot --eval-index <database.db> <evaluation.json>"
                );
                return;
            }

            if let Err(error) =
                knowledge::run_index_evaluation(Path::new(&args[1]), Path::new(&args[2]))
            {
                eprintln!("SQLite evaluation failed: {error}");
            }
        }

        _ => {
            let incoming = bot::IncomingMessage {
                sender: "test-user".to_string(),
                text: args.join(" "),
                group_id: None,
            };

            let outgoing = bot::handle_message(&incoming);

            println!("Destination: {:?}", outgoing.destination);
            println!("Reply: {}", outgoing.text);
        }
    }
}
