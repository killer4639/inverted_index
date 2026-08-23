mod doc_validator;
mod index_builder;
mod inverted_index;

use std::io::{self, Write};
use std::process;

use crate::index_builder::IndexBuilder;
use crate::inverted_index::InvertedIndex;
use doc_validator::validate_doc;

fn main() {
    let stdin = io::stdin();
    let mut index_builder = Some(IndexBuilder::new());
    let mut inverted_index: Option<InvertedIndex> = None;

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
            (Some("finalize"), None) => match finalize_index(&mut index_builder) {
                Ok(index) => {
                    println!(
                        "finalized {} document(s), {} term(s), {} posting(s)",
                        index.document_count(),
                        index.term_count(),
                        index.posting_count()
                    );
                    inverted_index = Some(index);
                    Ok(())
                }
                Err(error) => Err(error),
            },
            (Some("query"), Some(word)) => match inverted_index.as_ref() {
                Some(index) => query_word(word, index),
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

fn finalize_index(index_builder: &mut Option<IndexBuilder>) -> Result<InvertedIndex, String> {
    let builder = match index_builder.take() {
        Some(builder) => builder,
        None => return Err("index is already finalized".to_owned()),
    };

    builder.finalize()
}

fn query_word(word: &str, inverted_index: &InvertedIndex) -> Result<(), String> {
    if !word
        .chars()
        .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("query must be one ASCII-alphanumeric word".to_owned());
    }

    let postings = inverted_index.query(word);
    println!("{} matching document(s)", postings.len());
    for posting in postings {
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

    #[test]
    fn documents_cannot_be_added_after_finalization() {
        let mut builder = Some(IndexBuilder::new());
        let _index = finalize_index(&mut builder).unwrap();

        let error = index_document("unused.txt", &mut builder).unwrap_err();

        assert_eq!(error, "cannot add documents after finalization");
    }
}
