mod bot;
mod context_eval;
mod document_map;
mod knowledge;
mod llm;
mod signal;
mod smart_rag;

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
        "--mock-daemon" => {
            if args.len() != 2 {
                eprintln!("Usage: signal-knowledge-bot --mock-daemon <socket-path>");
                std::process::exit(1);
            }

            if let Err(error) = signal::run_mock_daemon(Path::new(&args[1])) {
                eprintln!("Mock daemon error: {error}");
                std::process::exit(1);
            }
        }

        "--socket-client" => {
            if args.len() != 3 {
                eprintln!(
                    "Usage: signal-knowledge-bot --socket-client <socket-path> <database.db>"
                );
                std::process::exit(1);
            }

            let instructions = load_answering_instructions();

            let index = match knowledge::SqliteIndex::open(Path::new(&args[2])) {
                Ok(index) => index,
                Err(error) => {
                    eprintln!("Failed to open index: {error}");
                    std::process::exit(1);
                }
            };

            let model = load_language_model();

            let result = signal::run_socket_client(Path::new(&args[1]), |incoming| {
                bot::handle_message_with_answer(incoming, |question| {
                    let started = Instant::now();

                    let results = index
                        .search(question, knowledge::MAX_PAGES, knowledge::MAX_CONTEXT_CHARS)
                        .map_err(std::io::Error::other)?;

                    eprintln!(
                        "Retrieved {} pages, {} context characters.",
                        results.pages.len(),
                        results.context.chars().count()
                    );

                    let answer = if smart_rag::enabled() {
                        smart_rag::answer(
                            &index,
                            model.as_ref(),
                            question,
                            &instructions,
                            results
                                .pages
                                .into_iter()
                                .map(|page| smart_rag::Page {
                                    document: page.source,
                                    number: page.page_number,
                                    text: page.text,
                                })
                                .collect(),
                        )
                    } else {
                        llm::answer_question(
                            model.as_ref(),
                            question,
                            results.pages.iter().map(|page| llm::Source {
                                document: &page.source,
                                page: page.page_number,
                                text: &page.text,
                            }),
                            &instructions,
                        )
                    };

                    eprintln!(
                        "Retrieval and answer stage: {:.2}s",
                        started.elapsed().as_secs_f64()
                    );

                    answer
                })
            });

            if let Err(error) = result {
                eprintln!("Socket client error: {error}");
                std::process::exit(1);
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

        "--contextualize-index" => {
            if args.len() < 2 {
                eprintln!(
                    "Usage: signal-knowledge-bot --contextualize-index <database.db> [--model MODEL] [--max-pages N]"
                );
                std::process::exit(1);
            }
            let path = Path::new(&args[1]);
            let mut model = "gemini-3.8-flash-low";
            let mut max_pages = 10usize;
            let mut position = 2;
            while position < args.len() {
                match args[position].as_str() {
                    "--model" => {
                        let Some(value) = args.get(position + 1) else {
                            eprintln!("Missing value for --model");
                            std::process::exit(1);
                        };
                        if value.trim().is_empty() || value.starts_with("--") {
                            eprintln!("Invalid model name");
                            std::process::exit(1);
                        }
                        model = value;
                        position += 2;
                    }
                    "--max-pages" => {
                        let Some(value) = args.get(position + 1) else {
                            eprintln!("Missing value for --max-pages");
                            std::process::exit(1);
                        };
                        max_pages = match value.parse::<usize>() {
                            Ok(n) if n > 0 && n <= 50 => n,
                            _ => {
                                eprintln!("--max-pages must be between 1 and 50");
                                std::process::exit(1);
                            }
                        };
                        position += 2;
                    }
                    other => {
                        eprintln!("Unknown contextualization option: {other}");
                        std::process::exit(1);
                    }
                }
            }
            let started = Instant::now();
            eprintln!(
                "Contextualizing {} using {model} (up to {max_pages} pages)",
                path.display()
            );
            match knowledge::SqliteIndex::contextualize_with_agy(path, model, max_pages) {
                Ok((pages, chunks)) => {
                    println!("Complete: {pages} PDF pages, {chunks} chunks contextualized");
                    println!("Elapsed: {:.2}s", started.elapsed().as_secs_f64());
                }
                Err(error) => {
                    eprintln!("Contextualization failed: {error}");
                    std::process::exit(1);
                }
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

        "--ask" => {
            if args.len() < 3 {
                eprintln!("Usage: signal-knowledge-bot --ask <database.db> <question>");
                std::process::exit(1);
            }

            let model = load_language_model();
            let question = args[2..].join(" ");

            if question.trim().is_empty() {
                eprintln!("The question cannot be empty.");
                std::process::exit(1);
            }

            let instructions = load_answering_instructions();

            let index = match knowledge::SqliteIndex::open(Path::new(&args[1])) {
                Ok(index) => index,
                Err(error) => {
                    eprintln!("Failed to open index: {error}");
                    std::process::exit(1);
                }
            };

            let results = match index.search(
                &question,
                knowledge::MAX_PAGES,
                knowledge::MAX_CONTEXT_CHARS,
            ) {
                Ok(results) => results,
                Err(error) => {
                    eprintln!("Retrieval failed: {error}");
                    std::process::exit(1);
                }
            };

            eprintln!(
                "Retrieved {} pages, {} context characters.",
                results.pages.len(),
                results.context.chars().count()
            );

            let started = Instant::now();

            let answer = if smart_rag::enabled() {
                smart_rag::answer(
                    &index,
                    model.as_ref(),
                    &question,
                    &instructions,
                    results
                        .pages
                        .into_iter()
                        .map(|page| smart_rag::Page {
                            document: page.source,
                            number: page.page_number,
                            text: page.text,
                        })
                        .collect(),
                )
            } else {
                llm::answer_question(
                    model.as_ref(),
                    &question,
                    results.pages.iter().map(|page| llm::Source {
                        document: &page.source,
                        page: page.page_number,
                        text: &page.text,
                    }),
                    &instructions,
                )
            };

            match answer {
                Ok(answer) => println!("{answer}"),
                Err(error) => {
                    eprintln!("Answer generation failed: {error}");
                    std::process::exit(1);
                }
            }

            eprintln!("Answer stage: {:.2}s", started.elapsed().as_secs_f64());
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

        "--document-catalog" => {
            if args.len() != 2 {
                eprintln!("Usage: signal-knowledge-bot --document-catalog <database.db>");
                std::process::exit(1);
            }
            if let Err(error) = document_map::catalog(Path::new(&args[1])) {
                eprintln!("Document catalog failed: {error}");
                std::process::exit(1);
            }
        }

        "--document-outline" => {
            if args.len() != 3 {
                eprintln!(
                    "Usage: signal-knowledge-bot --document-outline <database.db> <document-filename>"
                );
                std::process::exit(1);
            }
            if let Err(error) = document_map::outline(Path::new(&args[1]), &args[2]) {
                eprintln!("Document outline failed: {error}");
                std::process::exit(1);
            }
        }

        "--eval-context" => {
            if args.len() != 3 {
                eprintln!(
                    "Usage: signal-knowledge-bot --eval-context <database.db> <evaluation.json>"
                );
                std::process::exit(1);
            }

            if let Err(error) = context_eval::run(Path::new(&args[1]), Path::new(&args[2])) {
                eprintln!("Contextual evaluation failed: {error}");
                std::process::exit(1);
            }
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

fn load_answering_instructions() -> String {
    let path = Path::new("config/answering.txt");

    match llm::load_instructions(path) {
        Ok(instructions) => instructions,
        Err(error) => {
            eprintln!("Failed to load {}: {error}", path.display());
            std::process::exit(1);
        }
    }
}

fn load_language_model() -> Box<dyn llm::LanguageModel> {
    match llm::load_model() {
        Ok(model) => model,
        Err(error) => {
            eprintln!("Failed to configure language model: {error}");
            std::process::exit(1);
        }
    }
}
