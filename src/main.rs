mod bot;
mod knowledge;
mod signal;

use std::path::Path;

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

            const MAX_PAGES: usize = 5;
            const MAX_CONTEXT_CHARS: usize = 20_000;

            match knowledge::load_pdf_directory(directory) {
                Ok(documents) => {
                    println!("Loaded {} PDF documents", documents.len());

                    let ranked = knowledge::search_documents(&documents, &question, MAX_PAGES);

                    let context = knowledge::build_bounded_page_context(&ranked, MAX_CONTEXT_CHARS);

                    if context.pages.is_empty() {
                        println!("No matching pages fit the context budget.");
                        return;
                    }

                    println!("Question: {question}");

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
