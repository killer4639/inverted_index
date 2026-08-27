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

const DEFAULT_SEGMENT_PATH: &str = "segment.idx";

fn main() {
    let stdin = io::stdin();
    let mut index_builder = Some(IndexBuilder::new());
    let mut query_engine: Option<QueryEngine> = None;

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

        let mut parts = line.split_whitespace();
        let result = match (parts.next(), parts.next()) {
            (Some("index"), Some(path)) => index_document(path, &mut index_builder),
            (Some("finalize"), None) => {
                match finalize_index(&mut index_builder, DEFAULT_SEGMENT_PATH) {
                    Ok((engine, document_count, term_count, posting_count)) => {
                        println!(
                            "finalized {document_count} document(s), {term_count} term(s), {posting_count} posting(s) to {DEFAULT_SEGMENT_PATH}"
                        );
                        query_engine = Some(engine);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            (Some("query"), Some(word)) => match query_engine.as_ref() {
                Some(engine) => query_word(word, engine),
                None => Err("index is not ready to be queried".to_owned()),
            },
            (None, _) => continue,
            _ => {
                eprintln!("usage:\n  index <path>\n  finalize\n  query <word>");
                process::exit(2);
            }
        };

        if let Err(error) = result {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn index_document(path: &str, index_builder: &mut Option<IndexBuilder>) -> Result<(), String> {
    let builder = match index_builder.as_mut() {
        Some(builder) => builder,
        None => return Err("cannot add documents after finalization".to_owned()),
    };

    validate_doc(path)?;
    builder.create_index(path)?;
    Ok(())
}

fn finalize_index(
    index_builder: &mut Option<IndexBuilder>,
    segment_path: &str,
) -> Result<(QueryEngine, u32, usize, usize), String> {
    let builder = match index_builder.take() {
        Some(builder) => builder,
        None => return Err("index is already finalized".to_owned()),
    };

    let index = builder.finalize()?;
    let document_count = index.document_count();
    let term_count = index.term_count();
    let posting_count = index.posting_count();

    if let Err(error) = index_codec::encode(segment_path, &index) {
        return Err(format!("failed to write segment: {error}"));
    }
    drop(index);

    match QueryEngine::new(segment_path) {
        Ok(engine) => Ok((engine, document_count, term_count, posting_count)),
        Err(error) => Err(format!("failed to open segment: {error}")),
    }
}

fn query_word(word: &str, query_engine: &QueryEngine) -> Result<(), String> {
    let decoder = match query_engine.query_term(word) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    fn test_segment_path() -> std::path::PathBuf {
        let id = TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "cli-inverted-index-segment-{}-{id}.idx",
            std::process::id()
        ))
    }

    #[test]
    fn documents_cannot_be_added_after_finalization() {
        let mut builder = Some(IndexBuilder::new());
        let path = test_segment_path();
        let (engine, _, _, _) = finalize_index(&mut builder, path.to_str().unwrap()).unwrap();

        let error = index_document("unused.txt", &mut builder).unwrap_err();

        assert_eq!(error, "cannot add documents after finalization");
        drop(engine);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn finalization_writes_the_segment() {
        let mut builder = Some(IndexBuilder::new());
        let path = test_segment_path();

        let (engine, document_count, term_count, posting_count) =
            finalize_index(&mut builder, path.to_str().unwrap()).unwrap();

        assert!(path.exists());
        assert_eq!(document_count, 0);
        assert_eq!(term_count, 0);
        assert_eq!(posting_count, 0);
        drop(engine);
        fs::remove_file(path).unwrap();
    }
}
