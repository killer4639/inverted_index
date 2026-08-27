mod doc_validator;
mod index_builder;
mod index_codec;
mod inverted_index;
mod postings_codec;
mod query_engine;
mod segment_reader;
mod varint;

use std::io::{self, Write};
use std::process;

use crate::index_builder::IndexBuilder;
use crate::query_engine::QueryEngine;
use doc_validator::validate_doc;

const USAGE: &str = "usage:
  build <corpus> <segment>
  lookup <segment> <term>";

fn main() {
    let stdin = io::stdin();
    let mut open_segment: Option<(String, QueryEngine)> = None;

    loop {
        print!("> ");
        if io::stdout().flush().is_err() {
            eprintln!("failed to write prompt");
            process::exit(1);
        }

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) => {
                eprintln!("failed to read command: {error}");
                process::exit(1);
            }
        }

        let arguments: Vec<&str> = line.split_whitespace().collect();
        let result = match arguments.as_slice() {
            ["build", corpus_path, segment_path] => build_segment(corpus_path, segment_path),
            ["lookup", segment_path, term] => lookup_term(segment_path, term, &mut open_segment),
            [] => continue,
            _ => {
                eprintln!("{USAGE}");
                continue;
            }
        };

        if let Err(error) = result {
            eprintln!("{error}");
        }
    }
}

fn build_segment(corpus_path: &str, segment_path: &str) -> Result<(), String> {
    validate_doc(corpus_path)?;

    let mut builder = IndexBuilder::new();
    builder.create_index(corpus_path)?;
    let index = builder.finalize()?;
    let document_count = index.document_count();
    let term_count = index.term_count();
    let posting_count = index.posting_count();

    if let Err(error) = index_codec::encode(segment_path, &index) {
        return Err(format!("failed to write segment: {error}"));
    }

    println!(
        "built {document_count} document(s), {term_count} term(s), \
         {posting_count} posting(s) into {segment_path}"
    );
    Ok(())
}

fn lookup_term(
    segment_path: &str,
    term: &str,
    open_segment: &mut Option<(String, QueryEngine)>,
) -> Result<(), String> {
    let should_open = match open_segment.as_ref() {
        Some((open_path, _)) => open_path != segment_path,
        None => true,
    };

    if should_open {
        let query_engine = match QueryEngine::new(segment_path) {
            Ok(engine) => engine,
            Err(error) => return Err(format!("failed to open segment: {error}")),
        };
        *open_segment = Some((segment_path.to_owned(), query_engine));
    }

    let query_engine = match open_segment.as_ref() {
        Some((_, query_engine)) => query_engine,
        None => return Err("segment is not open".to_owned()),
    };
    let decoder = match query_engine.query_term(term) {
        Ok(Some(decoder)) => decoder,
        Ok(None) => {
            println!("0 matching document(s)");
            return Ok(());
        }
        Err(error) => return Err(error.to_string()),
    };

    println!("{} matching document(s)", decoder.remaining_postings());
    for result in decoder {
        let posting = match result {
            Ok(posting) => posting,
            Err(error) => return Err(format!("failed to decode postings: {error:?}")),
        };
        println!(
            "document {}: frequency {}",
            posting.document_id, posting.term_frequency
        );
    }

    Ok(())
}
